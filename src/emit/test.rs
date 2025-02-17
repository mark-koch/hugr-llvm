use crate::custom::int::add_int_extensions;
use crate::fat::FatExt as _;
use hugr::builder::{
    BuildHandle, Container, DFGWrapper, Dataflow, HugrBuilder, ModuleBuilder, SubContainer,
};
use hugr::extension::prelude::BOOL_T;
use hugr::extension::{ExtensionRegistry, ExtensionSet, EMPTY_REG};
use hugr::ops::constant::CustomConst;
use hugr::ops::handle::FuncID;
use hugr::ops::{Module, Tag, UnpackTuple, Value};
use hugr::std_extensions::arithmetic::int_ops::{self, INT_OPS_REGISTRY};
use hugr::std_extensions::arithmetic::int_types::ConstInt;
use hugr::types::{Type, TypeRow};
use hugr::Hugr;
use hugr::{builder::DataflowSubContainer, types::FunctionType};
use inkwell::passes::PassManager;
use insta::assert_snapshot;
use itertools::Itertools;
use rstest::rstest;

use crate::test::*;

#[allow(clippy::upper_case_acronyms)]
type DFGW<'a> = DFGWrapper<&'a mut Hugr, BuildHandle<FuncID<true>>>;

struct SimpleHugrConfig {
    ins: TypeRow,
    outs: TypeRow,
    extensions: ExtensionRegistry,
}

impl SimpleHugrConfig {
    pub fn new() -> Self {
        Self {
            ins: Default::default(),
            outs: Default::default(),
            extensions: EMPTY_REG,
        }
    }

    pub fn with_ins(mut self, ins: impl Into<TypeRow>) -> Self {
        self.ins = ins.into();
        self
    }

    pub fn with_outs(mut self, outs: impl Into<TypeRow>) -> Self {
        self.outs = outs.into();
        self
    }

    pub fn with_extensions(mut self, extensions: ExtensionRegistry) -> Self {
        self.extensions = extensions;
        self
    }

    pub fn finish(
        self,
        make: impl for<'a> FnOnce(DFGW<'a>) -> <DFGW<'a> as SubContainer>::ContainerHandle,
    ) -> Hugr {
        self.finish_with_exts(|builder, _| make(builder))
    }
    pub fn finish_with_exts(
        self,
        make: impl for<'a> FnOnce(
            DFGW<'a>,
            &ExtensionRegistry,
        ) -> <DFGW<'a> as SubContainer>::ContainerHandle,
    ) -> Hugr {
        let mut mod_b = ModuleBuilder::new();
        let func_b = mod_b
            .define_function("main", FunctionType::new(self.ins, self.outs).into())
            .unwrap();
        make(func_b, &self.extensions);
        mod_b.finish_hugr(&self.extensions).unwrap()
    }
}

macro_rules! check_emission {
    ($hugr: ident, $test_ctx:ident) => {
        let root = $hugr.fat_root::<Module>().unwrap();
        let (_, module) = $test_ctx.with_emit_context(|ec| ((), ec.emit_module(root).unwrap()));

        let mut settings = insta::Settings::clone_current();
        let new_suffix = settings
            .snapshot_suffix()
            .map_or("pre-mem2reg".into(), |x| format!("pre-mem2reg@{x}"));
        settings.set_snapshot_suffix(new_suffix);
        settings.bind(|| assert_snapshot!(module.to_string()));

        module
            .verify()
            .unwrap_or_else(|pp| panic!("Failed to verify module: {pp}"));

        let pb = PassManager::create(());
        pb.add_promote_memory_to_register_pass();
        pb.run_on(&module);

        assert_snapshot!(module.to_string());
    };
}

#[rstest]
fn emit_hugr_make_tuple(llvm_ctx: TestContext) {
    let hugr = SimpleHugrConfig::new()
        .with_ins(vec![BOOL_T, BOOL_T])
        .with_outs(Type::new_tuple(vec![BOOL_T, BOOL_T]))
        .finish(|mut builder: DFGW| {
            let in_wires = builder.input_wires();
            let r = builder.make_tuple(in_wires).unwrap();
            builder.finish_with_outputs([r]).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_unpack_tuple(llvm_ctx: TestContext) {
    let hugr = SimpleHugrConfig::new()
        .with_ins(Type::new_tuple(vec![BOOL_T, BOOL_T]))
        .with_outs(vec![BOOL_T, BOOL_T])
        .finish(|mut builder: DFGW| {
            let unpack = builder
                .add_dataflow_op(
                    UnpackTuple::new(vec![BOOL_T, BOOL_T].into()),
                    builder.input_wires(),
                )
                .unwrap();
            builder.finish_with_outputs(unpack.outputs()).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_tag(llvm_ctx: TestContext) {
    let hugr = SimpleHugrConfig::new()
        .with_outs(Type::new_unit_sum(3))
        .finish(|mut builder: DFGW| {
            let tag = builder
                .add_dataflow_op(
                    Tag::new(1, vec![vec![].into(), vec![].into(), vec![].into()]),
                    builder.input_wires(),
                )
                .unwrap();
            builder.finish_with_outputs(tag.outputs()).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_dfg(llvm_ctx: TestContext) {
    let hugr = SimpleHugrConfig::new()
        .with_ins(Type::UNIT)
        .with_outs(Type::UNIT)
        .finish(|mut builder: DFGW| {
            let dfg = {
                let b = builder
                    .dfg_builder(
                        FunctionType::new_endo(Type::UNIT),
                        None,
                        builder.input_wires(),
                    )
                    .unwrap();
                let w = b.input_wires();
                b.finish_with_outputs(w).unwrap()
            };
            builder.finish_with_outputs(dfg.outputs()).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_conditional(llvm_ctx: TestContext) {
    let hugr = {
        let input_v_rows: Vec<TypeRow> = (0..3).map(Type::new_unit_sum).map_into().collect_vec();
        let output_v_rows = {
            let mut r = input_v_rows.clone();
            r.reverse();
            r
        };

        SimpleHugrConfig::new()
            .with_ins(vec![Type::new_sum(input_v_rows.clone()), Type::UNIT])
            .with_outs(vec![Type::new_sum(output_v_rows.clone()), Type::UNIT])
            .finish(|mut builder: DFGW| {
                let cond = {
                    let [sum_input, other_input] = builder.input_wires_arr();
                    let mut cond_b = builder
                        .conditional_builder(
                            (input_v_rows.clone(), sum_input),
                            [(Type::UNIT, other_input)],
                            vec![Type::new_sum(output_v_rows.clone()), Type::UNIT].into(),
                            ExtensionSet::default(),
                        )
                        .unwrap();
                    for i in 0..3 {
                        let mut case_b = cond_b.case_builder(i).unwrap();
                        let [case_input, other_input] = case_b.input_wires_arr();
                        let tag = case_b
                            .add_dataflow_op(Tag::new(2 - i, output_v_rows.clone()), [case_input])
                            .unwrap();
                        case_b
                            .finish_with_outputs([tag.out_wire(0), other_input])
                            .unwrap();
                    }
                    cond_b.finish_sub_container().unwrap()
                };
                let [o1, o2] = cond.outputs_arr();
                builder.finish_with_outputs([o1, o2]).unwrap()
            })
    };
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_load_constant(#[with(-1, add_int_extensions)] llvm_ctx: TestContext) {
    let v = Value::tuple([
        Value::unit_sum(2, 4).unwrap(),
        ConstInt::new_s(4, -24).unwrap().into(),
    ]);

    let hugr = SimpleHugrConfig::new()
        .with_outs(v.get_type())
        .with_extensions(INT_OPS_REGISTRY.to_owned())
        .finish(|mut builder: DFGW| {
            let konst = builder.add_load_value(v);
            builder.finish_with_outputs([konst]).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}

#[rstest]
fn emit_hugr_custom_op(#[with(-1, add_int_extensions)] llvm_ctx: TestContext) {
    let v1 = ConstInt::new_s(4, -24).unwrap();
    let v2 = ConstInt::new_s(4, 24).unwrap();

    let hugr = SimpleHugrConfig::new()
        .with_outs(v1.get_type())
        .with_extensions(INT_OPS_REGISTRY.to_owned())
        .finish_with_exts(|mut builder: DFGW, ext_reg| {
            let k1 = builder.add_load_value(v1);
            let k2 = builder.add_load_value(v2);
            let ext_op = int_ops::EXTENSION
                .instantiate_extension_op("iadd", [4.into()], ext_reg)
                .unwrap();
            let add = builder.add_dataflow_op(ext_op, [k1, k2]).unwrap();
            builder.finish_with_outputs(add.outputs()).unwrap()
        });
    check_emission!(hugr, llvm_ctx);
}
