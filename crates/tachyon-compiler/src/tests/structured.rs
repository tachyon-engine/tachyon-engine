use super::*;

#[test]
/// Confirms control-flow HIR owns every nested node after the Oxc arena is dropped.
fn hir_owns_nested_block_if_and_throw_statements() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "if (false) { let hidden = 1; } else { throw 7; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one conditional statement");
    };
    let HirStatementKind::If {
        consequent,
        alternate: Some(alternate),
        ..
    } = &statement.kind
    else {
        panic!("expected owned conditional arms");
    };
    assert!(matches!(consequent.kind, HirStatementKind::Block(_)));
    assert!(matches!(alternate.kind, HirStatementKind::Block(_)));
    let HirStatementKind::Block(statements) = &alternate.kind else {
        unreachable!();
    };
    assert!(matches!(statements[0].kind, HirStatementKind::Throw(_)));
}

#[test]
/// Confirms switch clause order, default placement, and break survive arena teardown.
fn hir_owns_switch_cases_in_source_order() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "switch (2) { case 1: break; default: 3; case 2: 4; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [statement] = hir.statements() else {
        panic!("expected one switch statement");
    };
    let HirStatementKind::Switch {
        discriminant,
        cases,
    } = &statement.kind
    else {
        panic!("expected owned switch HIR");
    };
    assert_eq!(
        discriminant.kind,
        HirExpressionKind::Number(2.0_f64.to_bits())
    );
    assert_eq!(cases.len(), 3);
    assert!(cases[0].test.is_some());
    assert!(matches!(
        cases[0].consequent[0].kind,
        HirStatementKind::Break
    ));
    assert!(cases[1].test.is_none());
    assert!(cases[2].test.is_some());
}

#[test]
/// Keeps pre-test and post-test loop ordering explicit after the Oxc arena is dropped.
fn hir_distinguishes_while_and_do_while_evaluation_order() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "while (false) { 1; } do { 2; } while (false);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [while_statement, do_while_statement] = hir.statements() else {
        panic!("expected two loop statements");
    };
    assert!(matches!(
        while_statement.kind,
        HirStatementKind::Loop {
            test_first: true,
            ..
        }
    ));
    assert!(matches!(
        do_while_statement.kind,
        HirStatementKind::Loop {
            test_first: false,
            ..
        }
    ));
}

#[test]
/// Owns declaration and assignment heads after Oxc's arena is dropped.
fn hir_owns_for_in_heads_and_body() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "let target; for (const key in { first: 1 }) { target = key; } for (target in {}) {}",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let [_, declaration_loop, assignment_loop] = hir.statements() else {
        panic!("expected declaration and two for-in statements");
    };
    assert!(matches!(
        declaration_loop.kind,
        HirStatementKind::ForIn {
            left: crate::hir::HirForInLeft::Variable(_),
            ..
        }
    ));
    assert!(matches!(
        assignment_loop.kind,
        HirStatementKind::ForIn {
            left: crate::hir::HirForInLeft::Assignment(_),
            ..
        }
    ));
}

#[test]
fn compiler_emits_verified_for_in_iterator_bytecode() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let result; for (let key in { first: 1 }) { result = key; } result;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("CreateForInIterator"));
    assert!(disassembly.contains("ForInNext"));
    assert!(disassembly.contains("JumpIfTrue"));
}

#[test]
/// Keeps synchronous `for...of` on the shared property/call iterator protocol.
fn compiler_emits_verified_for_of_iterator_bytecode() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let result = 0; for (let value of [1, 2]) { result += value; } result;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("CallWithReceiver"));
    assert!(disassembly.contains("GetByValue"));
    assert!(disassembly.contains("JumpIfTrue"));
}

#[test]
/// Owns ordinary object data properties and rejects descriptor-bearing syntax at the boundary.
fn hir_owns_plain_object_literal_properties() {
    let hir = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "({ answer: 40, label: 'ok' });"),
            CompileOptions::default(),
        )
        .unwrap();
    let Some(HirStatement {
        kind:
            HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Object(properties),
                ..
            }),
        ..
    }) = hir.statements().first()
    else {
        panic!("expected owned object literal");
    };
    assert_eq!(properties.len(), 2);
    assert!(matches!(
        &properties[0].key,
        HirObjectPropertyKey::Static(key) if key.as_ref() == "answer"
    ));
    assert!(matches!(
        &properties[1].key,
        HirObjectPropertyKey::Static(key) if key.as_ref() == "label"
    ));
    let computed = Compiler
        .lower_to_hir(
            source(MediaType::JavaScript, "({ ['answer']: 40 });"),
            CompileOptions::default(),
        )
        .unwrap();
    assert!(matches!(
        computed.statements().first(),
        Some(HirStatement {
            kind: HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::Object(properties),
                ..
            }),
            ..
        }) if matches!(properties[0].key, HirObjectPropertyKey::Computed(_))
    ));
    assert!(matches!(
        Compiler
            .lower_to_hir(
                source(MediaType::JavaScript, "({ ...other });"),
                CompileOptions::default(),
            )
            .unwrap()
            .statements()
            .first(),
        Some(HirStatement {
            kind: HirStatementKind::Expression(HirExpression {
                kind: HirExpressionKind::ObjectSpread(_),
                ..
            }),
            ..
        })
    ));
}

#[test]
/// Confirms structured entry lowering produces verifier-accepted branch and abrupt opcodes.
fn compiler_emits_verified_top_level_branch_and_throw() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "if (false) { 1; } else { throw 7; }"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("JumpIfFalse"));
    assert!(disassembly.contains("Throw"));
}

#[test]
fn compiler_emits_each_logical_short_circuit_branch() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "0 && 1; 0 || 2; null ?? 3;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("JumpIfFalse"));
    assert!(disassembly.contains("JumpIfTrue"));
    assert!(disassembly.contains("JumpIfNotNullish"));
}

#[test]
fn compiler_emits_verified_switch_dispatch_and_break_targets() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "switch (2) { case 1: break; default: 3; case 2: break; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disassembly.matches("StrictEqual").count(), 2);
    assert_eq!(disassembly.matches("JumpIfTrue").count(), 2);
    assert!(disassembly.matches("Jump").count() >= 4);
}

#[test]
fn compiler_marks_global_lexical_const_as_immutable() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "const answer = 42; answer = 0;"),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert!(entry.binding_plan().iter().any(|binding| {
        binding.name.as_ref() == "answer"
            && binding.location == tachyon_bytecode::BindingLocation::GlobalLexical
            && !binding.mutable
    }));
}

#[test]
fn compiler_freezes_directive_and_inherited_function_strictness() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function sloppy() {} function strict() { 'use strict'; function nested() {} }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Sloppy
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(1))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Sloppy
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(2))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
    assert_eq!(
        module
            .function(tachyon_bytecode::FunctionId::new(3))
            .unwrap()
            .strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
}

#[test]
fn unused_parameters_remain_reserved_in_the_verified_frame_layout() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "function pair(first, second) {}"),
            CompileOptions::default(),
        )
        .unwrap();
    let layout = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap()
        .layout();
    assert_eq!(layout.argument_count, 2);
    assert_eq!(layout.function_length, 2);
    assert_eq!(layout.register_count, 2);
}

#[test]
fn compiler_lowers_compound_assignment_through_the_shared_binary_path() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "let value = 1; value += 2;"),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("Add"));
}

#[test]
fn compiler_freezes_nested_try_catch_ranges_and_exact_depth() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "try { try { throw 1; } catch (inner) { throw inner; } } catch (outer) { outer; }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 2);
    assert_eq!(function.layout().max_handler_depth, 2);
    let outer = function.handlers()[0];
    let inner = function.handlers()[1];
    assert!(outer.protected_start.index() < inner.protected_start.index());
    assert!(inner.protected_end.index() < outer.protected_end.index());
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("LoadException").count(), 2);
}

#[test]
/// Locks the single-body finalizer shape and exact completion-depth metadata.
fn compiler_lowers_finally_to_explicit_completion_replay() {
    let module = Compiler
        .compile(
            source(MediaType::JavaScript, "try { 1; } finally { 2; }"),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 1);
    assert_eq!(
        function.handlers()[0].kind,
        tachyon_bytecode::HandlerKind::Finally
    );
    assert_eq!(function.layout().max_handler_depth, 1);
    assert_eq!(function.layout().max_completion_depth, 1);
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("EnterFinally").count(), 1);
    assert_eq!(disassembly.matches("ResumeCompletion").count(), 1);
}

#[test]
/// Proves catch remains inner to finally and escaping loop control uses an abrupt target.
fn compiler_lowers_catch_finally_and_break_without_copying_the_finalizer() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "while (true) { try { throw 1; } catch (error) { break; } finally { 2; } }",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(0))
        .unwrap();
    assert_eq!(function.handlers().len(), 2);
    assert_eq!(
        function.handlers()[0].kind,
        tachyon_bytecode::HandlerKind::Finally
    );
    assert_eq!(
        function.handlers()[1].kind,
        tachyon_bytecode::HandlerKind::Catch
    );
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("BreakThroughFinally").count(), 1);
    assert_eq!(disassembly.matches("ResumeCompletion").count(), 1);
}

#[test]
/// Keeps a structural terminal after a function's necessarily abrupt completion replay.
fn compiler_terminates_function_return_through_finally() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function f() { try { return 7; } finally { 1; } } f();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = module
        .function(tachyon_bytecode::FunctionId::new(1))
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert!(disassembly.ends_with("ReturnUndefined\n"));
    let handler = function.handlers()[0];
    assert_eq!(handler.kind, tachyon_bytecode::HandlerKind::Finally);
    assert!(handler.handler_end.index() < function.bytecode().bytecode().words().len() as u32);
}

#[test]
fn compiler_preserves_static_member_receiver_and_property_names() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let object; object.value = 1; object.method(2);",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let disassembly = tachyon_bytecode::disassemble(
        module
            .function(tachyon_bytecode::FunctionId::new(0))
            .unwrap(),
    )
    .unwrap();
    assert!(disassembly.contains("SetById"));
    assert!(disassembly.contains("GetById"));
    assert!(disassembly.contains("CallWithReceiver"));
    assert!(
        module
            .scope_names()
            .iter()
            .any(|name| name.as_ref() == "value")
    );
    assert!(
        module
            .scope_names()
            .iter()
            .any(|name| name.as_ref() == "method")
    );
}

#[test]
fn compiler_owns_anonymous_and_nested_function_expression_stencils() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "let outer = function () { return function () { return 42; }; }; outer()();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(hir.functions().len(), 2);
    assert!(
        hir.functions()
            .iter()
            .all(|function| function.name.is_none())
    );
    assert_eq!(hir.functions()[0].id.index(), 0);
    assert_eq!(hir.functions()[1].id.index(), 1);
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "let outer = function () { return function () { return 42; }; }; outer()();",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(module.functions().len(), 3);
}

#[test]
/// Freezes generator identity independently from each later suspension point.
fn compiler_freezes_generator_function_kind() {
    let module_source = source(
        MediaType::JavaScript,
        "function* values() { return 1; } values;",
    );
    let hir = Compiler
        .lower_to_hir(module_source.clone(), CompileOptions::default())
        .unwrap();
    let [function] = hir.functions() else {
        panic!("expected one generator function stencil");
    };
    assert_eq!(function.kind, HirFunctionKind::Generator);

    let module = Compiler
        .compile(module_source, CompileOptions::default())
        .unwrap();
    assert_eq!(module.functions().len(), 2);
    assert_eq!(
        module.functions()[1].kind(),
        tachyon_bytecode::FunctionKind::Generator
    );
    assert!(
        tachyon_bytecode::disassemble(&module.functions()[1])
            .unwrap()
            .contains("Return")
    );
}

#[test]
/// Publishes verified resume metadata for ordinary and delegated generator suspension.
fn compiler_emits_generator_yield_suspend_points() {
    let module_source = source(
        MediaType::JavaScript,
        "function* values() { var first = yield 1; return yield first; } values;",
    );
    let hir = Compiler
        .lower_to_hir(module_source.clone(), CompileOptions::default())
        .unwrap();
    let [function] = hir.functions() else {
        panic!("expected one generator function stencil");
    };
    assert!(matches!(
        function.body[0].kind,
        HirStatementKind::VariableDeclaration(_)
    ));
    let module = Compiler
        .compile(module_source, CompileOptions::default())
        .unwrap();
    let function = &module.functions()[1];
    assert_eq!(function.suspend_points().len(), 2);
    for (index, point) in function.suspend_points().iter().enumerate() {
        assert_eq!(point.id.index(), index as u32);
        let instruction = tachyon_bytecode::decode_instruction(
            function.bytecode().bytecode().words(),
            point.instruction,
        )
        .unwrap();
        assert_eq!(instruction.opcode, tachyon_bytecode::Opcode::Yield);
        assert_eq!(instruction.operands[1], point.destination.index());
        assert_eq!(instruction.operands[2], point.id.index());
        assert_eq!(
            point.resume_offset.index(),
            point.instruction.index() + u32::from(instruction.word_len)
        );
    }
    let disassembly = tachyon_bytecode::disassemble(function).unwrap();
    assert_eq!(disassembly.matches("InitialYield").count(), 1);
    assert!(disassembly.contains("Yield"));

    let delegated = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "function* delegated(values) { return yield* values; } delegated;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let function = &delegated.functions()[1];
    let [point] = function.suspend_points() else {
        panic!("expected one delegated suspend point");
    };
    let instruction = tachyon_bytecode::decode_instruction(
        function.bytecode().bytecode().words(),
        point.instruction,
    )
    .unwrap();
    assert_eq!(instruction.opcode, tachyon_bytecode::Opcode::YieldWithKind);
    assert_eq!(instruction.operands[1], point.destination.index());
    assert!(instruction.operands[1] + 1 < function.layout().register_count);
    assert_eq!(instruction.operands[2], point.id.index());
    assert!(
        tachyon_bytecode::disassemble(function)
            .unwrap()
            .contains("YieldWithKind")
    );
    assert_eq!(
        tachyon_bytecode::disassemble(function)
            .unwrap()
            .matches("InitialYield")
            .count(),
        1
    );
}

#[test]
/// Freezes ordinary async and async generators as distinct callable function kinds.
fn compiler_accepts_async_functions_and_generators_as_distinct_kinds() {
    let async_module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "async function value() { return 1; } value;",
            ),
            CompileOptions::default(),
        )
        .expect("async function fixture compiles");
    assert_eq!(
        async_module.functions()[1].kind(),
        tachyon_bytecode::FunctionKind::Async
    );
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "async function* values() { yield 1; return 2; } values;",
            ),
            CompileOptions::default(),
        )
        .expect("async generator fixture compiles");
    assert_eq!(
        module.functions()[1].kind(),
        tachyon_bytecode::FunctionKind::AsyncGenerator
    );
    let function = &module.functions()[1];
    let disassembly = tachyon_bytecode::disassemble(function).expect("bytecode disassembles");
    assert_eq!(disassembly.matches("InitialYield").count(), 1);
    assert_eq!(disassembly.matches("Await").count(), 3);
    assert!(disassembly.contains("YieldWithKind"));
    assert!(disassembly.contains("Throw"));
    assert!(disassembly.contains("Return"));
    assert_eq!(function.suspend_points().len(), 4);
}

#[test]
/// Confirms async `for await...of` emits acquisition, iteration, and close suspension.
fn compiler_lowers_async_for_await_of_iterator_step() {
    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "async function collect(values) { var out = []; for await (const value of values) out.push(value); return out; } collect;",
            ),
            CompileOptions::default(),
        )
        .expect("async for-await-of fixture compiles");
    let function = module
        .functions()
        .iter()
        .find(|function| function.kind() == tachyon_bytecode::FunctionKind::Async)
        .expect("async function is published");
    let disassembly = tachyon_bytecode::disassemble(function).expect("bytecode disassembles");
    assert!(disassembly.contains("LoadAsyncIteratorSymbol"));
    assert!(disassembly.contains("LoadIteratorSymbol"));
    assert!(disassembly.contains("CreateAsyncFromSyncIterator"));
    assert!(disassembly.contains("Await"));
    assert!(disassembly.contains("EnterFinally"));
    assert!(disassembly.contains("ResumeCompletion"));
    assert_eq!(function.suspend_points().len(), 2);
    assert_eq!(function.handlers().len(), 1);
    assert_eq!(
        function.handlers()[0].kind,
        tachyon_bytecode::HandlerKind::IteratorClose
    );
}

#[test]
fn compiler_freezes_derived_class_and_super_call_contracts() {
    let hir = Compiler
        .lower_to_hir(
            source(
                MediaType::JavaScript,
                "class P extends Promise { constructor(executor) { return super(executor); } } new P(function() {});",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(hir.functions().len(), 2);
    assert_eq!(
        hir.functions()[0].kind,
        HirFunctionKind::DerivedClassConstructor
    );
    assert!(hir.functions()[0].strict);

    let module = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "class P extends Promise { constructor(executor) { return super(executor); } } new P(function() {});",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert_eq!(
        module.functions()[1].kind(),
        tachyon_bytecode::FunctionKind::DerivedClassConstructor
    );
    assert_eq!(
        module.functions()[1].strictness(),
        tachyon_bytecode::FunctionStrictness::Strict
    );
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    let constructor = tachyon_bytecode::disassemble(&module.functions()[1]).unwrap();
    assert!(entry.contains("CheckConstructor"));
    assert!(entry.contains("CreateClass"));
    assert!(constructor.contains("SuperConstruct"));
    assert!(constructor.contains("InitializeThis"));
}

#[test]
/// Freezes method kind, strictness, and installation opcodes independently of VM behavior.
fn compiler_freezes_class_method_kind_and_definition_contracts() {
    let source = source(
        MediaType::JavaScript,
        "class P extends Promise { constructor(executor) { super(executor); } value() { return this; } static make() { return this; } } P;",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    assert_eq!(hir.functions().len(), 3);
    assert_eq!(
        hir.functions()[0].kind,
        HirFunctionKind::DerivedClassConstructor
    );
    assert!(hir.functions()[1..].iter().all(|function| {
        function.kind == HirFunctionKind::Ordinary
            && function.role == HirFunctionRole::Method
            && function.strict
    }));

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    assert_eq!(module.functions().len(), 4);
    assert!(module.functions()[2..].iter().all(|function| {
        function.kind() == tachyon_bytecode::FunctionKind::Ordinary
            && function.role() == tachyon_bytecode::FunctionRole::Method
    }));
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("DefineClassMethodById").count(), 2);
}

#[test]
/// Keeps class placement orthogonal to ordinary, generator, async, and async-generator execution.
fn compiler_preserves_public_and_private_class_method_execution_kinds() {
    let source = source(
        MediaType::JavaScript,
        "class C { ordinary() {} *generator() { yield 1; } async asynchronous() { await 1; } async *asyncGenerator() { yield await 1; } #privateOrdinary() {} *#privateGenerator() { yield 1; } async #privateAsync() { await 1; } async *#privateAsyncGenerator() { yield await 1; } } C;",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    let expected = [
        ("ordinary", HirFunctionKind::Ordinary),
        ("generator", HirFunctionKind::Generator),
        ("asynchronous", HirFunctionKind::Async),
        ("asyncGenerator", HirFunctionKind::AsyncGenerator),
        ("#privateOrdinary", HirFunctionKind::Ordinary),
        ("#privateGenerator", HirFunctionKind::Generator),
        ("#privateAsync", HirFunctionKind::Async),
        ("#privateAsyncGenerator", HirFunctionKind::AsyncGenerator),
    ];
    for (name, kind) in expected {
        let function = hir
            .functions()
            .iter()
            .find(|function| function.name.as_deref() == Some(name))
            .unwrap();
        assert_eq!(function.kind, kind, "HIR execution kind for {name}");
        assert_eq!(
            function.role,
            HirFunctionRole::Method,
            "HIR role for {name}"
        );
        assert!(function.strict, "class method {name} is strict");
    }

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let methods: Vec<_> = module
        .functions()
        .iter()
        .filter(|function| function.role() == tachyon_bytecode::FunctionRole::Method)
        .collect();
    assert_eq!(methods.len(), expected.len());
    for kind in [
        tachyon_bytecode::FunctionKind::Ordinary,
        tachyon_bytecode::FunctionKind::Generator,
        tachyon_bytecode::FunctionKind::Async,
        tachyon_bytecode::FunctionKind::AsyncGenerator,
    ] {
        assert_eq!(
            methods
                .iter()
                .filter(|function| function.kind() == kind)
                .count(),
            2,
            "public and private methods preserve {kind:?}"
        );
    }
}

#[test]
/// Keeps object-method execution kind orthogonal to its independently published home object.
fn compiler_preserves_object_method_roles_and_home_objects() {
    let source = source(
        MediaType::JavaScript,
        "({ ordinary() { return super.value; }, *generator() { yield super.value; }, async asynchronous() { return super.value; }, async *asyncGenerator() { yield super.value; }, get value() { return super.value; }, set value(next) {} });",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    assert_eq!(hir.functions().len(), 6);
    assert!(
        hir.functions()
            .iter()
            .all(|function| function.role == HirFunctionRole::Method)
    );

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    assert!(
        module.functions()[1..]
            .iter()
            .all(|function| function.role() == tachyon_bytecode::FunctionRole::Method)
    );
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("SetFunctionHomeObject").count(), 6);
}

#[test]
/// Freezes the enter/initialize/leave protocol and method capture for named class expressions.
fn compiler_handles_named_class_expression_without_leaking_binding() {
    let class_source = source(
        MediaType::JavaScript,
        "var value = class Hidden { method() { return 1; } }; value.name;",
    );
    let hir = Compiler
        .lower_to_hir(class_source.clone(), CompileOptions::default())
        .unwrap();
    assert!(hir.statements().iter().any(|statement| {
        matches!(
            &statement.kind,
            HirStatementKind::VariableDeclaration(declaration)
                if declaration.declarators.iter().any(|declarator| {
                    matches!(
                        declarator.initializer.as_ref().map(|initializer| &initializer.kind),
                        Some(HirExpressionKind::Class(class))
                            if class.name.as_deref() == Some("Hidden")
                    )
                })
        )
    }));
    let module = Compiler
        .compile(class_source, CompileOptions::default())
        .unwrap();
    assert!(
        tachyon_bytecode::disassemble(&module.functions()[0])
            .unwrap()
            .contains("SetFunctionName")
    );

    let recursive = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "var value = class Hidden { method() { return Hidden; } }; value;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let entry = tachyon_bytecode::disassemble(&recursive.functions()[0]).unwrap();
    assert!(entry.contains("EnterClassEnvironment"));
    assert!(entry.contains("InitializeClassEnvironment"));
    assert!(entry.contains("LeaveClassEnvironment"));
    let method = recursive
        .functions()
        .iter()
        .find(|function| function.role() == tachyon_bytecode::FunctionRole::Method)
        .unwrap();
    assert!(method.binding_plan().iter().any(|binding| {
        matches!(
            binding.location,
            tachyon_bytecode::BindingLocation::ClassEnvironment { depth: 0, slot: 0 }
        )
    }));
    assert!(
        tachyon_bytecode::disassemble(method)
            .unwrap()
            .contains("LoadEnvironment r0, depth=0, slot=0")
    );

    let nested = Compiler
        .compile(
            source(
                MediaType::JavaScript,
                "var value = class Hidden { method() { let captured = 1; return function() { return captured && Hidden; }; } }; value;",
            ),
            CompileOptions::default(),
        )
        .unwrap();
    assert!(nested.functions().iter().any(|function| {
        function.binding_plan().iter().any(|binding| {
            binding.name.as_ref() == "Hidden"
                && matches!(
                    binding.location,
                    tachyon_bytecode::BindingLocation::ClassEnvironment { depth: 1, slot: 0 }
                )
        })
    }));
}

#[test]
/// Freezes the synthetic default-derived constructor as explicit forwarding bytecode.
fn compiler_emits_default_derived_constructor_forwarding() {
    let source = source(
        MediaType::JavaScript,
        "class P extends Promise { value() { return 1; } } new P(function() {});",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    assert!(hir.functions().iter().any(|function| {
        function.kind == HirFunctionKind::DefaultDerivedConstructor && function.strict
    }));

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let constructor = module
        .functions()
        .iter()
        .find(|function| function.kind() == tachyon_bytecode::FunctionKind::DerivedClassConstructor)
        .unwrap();
    let constructor = tachyon_bytecode::disassemble(constructor).unwrap();
    assert!(constructor.contains("SuperConstructForwardAll"));
    assert!(constructor.contains("InitializeThis"));
}

#[test]
/// Freezes explicit/default base constructors without derived-only initialization bytecode.
fn compiler_freezes_base_class_constructor_contracts() {
    let explicit = source(
        MediaType::JavaScript,
        "class A { constructor(value) { this.value = value; } } new A(1);",
    );
    let hir = Compiler
        .lower_to_hir(explicit.clone(), CompileOptions::default())
        .unwrap();
    assert_eq!(
        hir.functions()[0].kind,
        HirFunctionKind::BaseClassConstructor
    );
    assert!(hir.functions()[0].strict);
    let module = Compiler
        .compile(explicit, CompileOptions::default())
        .unwrap();
    assert_eq!(
        module.functions()[1].kind(),
        tachyon_bytecode::FunctionKind::BaseClassConstructor
    );
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert!(entry.contains("CreateBaseClass"));
    assert!(!entry.contains("CheckConstructor"));

    let default = source(MediaType::JavaScript, "class A {} new A();");
    let hir = Compiler
        .lower_to_hir(default.clone(), CompileOptions::default())
        .unwrap();
    assert_eq!(
        hir.functions()[0].kind,
        HirFunctionKind::DefaultBaseConstructor
    );
    let module = Compiler
        .compile(default, CompileOptions::default())
        .unwrap();
    let constructor = tachyon_bytecode::disassemble(&module.functions()[1]).unwrap();
    assert!(constructor.contains("ReturnUndefined"));
    assert!(!constructor.contains("SuperConstruct"));
}

#[test]
/// Freezes class accessors as strict methods installed through non-enumerable accessor opcodes.
fn compiler_freezes_class_accessor_contracts() {
    let source = source(
        MediaType::JavaScript,
        "class A { get value() { return this._value; } set value(v) { this._value = v; } static get answer() { return 42; } } A;",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    assert_eq!(hir.functions().len(), 4);
    let methods: Vec<_> = hir
        .functions()
        .iter()
        .filter(|function| function.role == HirFunctionRole::Method)
        .collect();
    assert_eq!(methods.len(), 3);
    assert!(methods.iter().all(|function| function.strict));
    assert!(
        methods
            .iter()
            .any(|function| function.name.as_deref() == Some("get value"))
    );
    assert!(
        methods
            .iter()
            .any(|function| function.name.as_deref() == Some("set value"))
    );

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let accessors: Vec<_> = module
        .functions()
        .iter()
        .filter(|function| function.role() == tachyon_bytecode::FunctionRole::Method)
        .collect();
    assert_eq!(accessors.len(), 3);
    assert!(accessors.iter().all(|function| {
        function.kind() == tachyon_bytecode::FunctionKind::Ordinary
            && function.strictness() == tachyon_bytecode::FunctionStrictness::Strict
    }));
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("DefineClassGetterById").count(), 2);
    assert_eq!(entry.matches("DefineClassSetterById").count(), 1);
}

#[test]
/// Preserves computed class-key expressions and emits runtime naming/value-definition opcodes.
fn compiler_freezes_computed_class_method_contracts() {
    let source = source(
        MediaType::JavaScript,
        "var key = 'value'; class A { [key]() { return 1; } static get [key]() { return 2; } } A;",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    let methods: Vec<_> = hir
        .functions()
        .iter()
        .filter(|function| function.role == HirFunctionRole::Method)
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(methods.iter().all(|function| function.name.is_none()));

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("ToPropertyKey").count(), 2);
    assert_eq!(entry.matches("SetFunctionNameByValue").count(), 1);
    assert_eq!(entry.matches("SetAccessorFunctionName").count(), 1);
    assert_eq!(entry.matches("DefineClassMethodByValue").count(), 1);
    assert_eq!(entry.matches("DefineClassGetterByValue").count(), 1);
}

#[test]
/// Freezes static/computed super reads and receiver-preserving calls as class-only opcodes.
fn compiler_freezes_super_property_contracts() {
    let source = source(
        MediaType::JavaScript,
        "class A { value() { return 1; } } class B extends A { value() { return super.value(); } other(key) { return super[key]; } } new B().value();",
    );
    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let disassembly = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .collect::<String>();
    assert!(disassembly.contains("GetSuperById"));
    assert!(disassembly.contains("LoadSuperBase"));
    assert!(disassembly.contains("GetSuperByValue"));
    assert!(disassembly.contains("CallWithReceiver"));
}

#[test]
/// Keeps a private member reference's base value as the dynamic receiver at its call site.
fn compiler_freezes_private_method_call_receiver() {
    let source = source(
        MediaType::JavaScript,
        "class C { #value = 1; #method(argument) { return this.#value + argument; } call() { return this.#method(2); } } new C().call();",
    );
    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let call_method = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .find(|function| function.contains("GetPrivate") && function.contains("CallWithReceiver"))
        .expect("private method call emits one receiver-preserving function");
    assert!(call_method.contains("GetPrivate"));
    assert!(!call_method.contains("Call r"));
}

#[test]
/// Merges a private getter/setter pair and freezes one shared accessor payload.
fn compiler_freezes_private_accessor_pair() {
    let source = source(
        MediaType::JavaScript,
        "class C { get #value() { return 1; } set #value(next) {} read() { return this.#value; } } C;",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    let class = hir
        .statements()
        .iter()
        .find_map(|statement| match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => declaration
                .declarators
                .iter()
                .find_map(|declarator| match declarator.initializer.as_ref() {
                    Some(HirExpression {
                        kind: HirExpressionKind::Class(class),
                        ..
                    }) => Some(class),
                    _ => None,
                }),
            _ => None,
        })
        .expect("class declaration retains one HIR class expression");
    let accessor = class
        .elements
        .iter()
        .find_map(|element| match element {
            HirClassElement::PrivateAccessor(accessor) => Some(accessor),
            _ => None,
        })
        .expect("private getter and setter merge into one HIR element");
    assert!(accessor.getter.is_some());
    assert!(accessor.setter.is_some());
    for function in hir
        .functions()
        .iter()
        .filter(|function| matches!(function.name.as_deref(), Some("get #value" | "set #value")))
    {
        assert_eq!(function.kind, HirFunctionKind::Ordinary);
        assert_eq!(function.role, HirFunctionRole::Method);
        assert!(function.strict);
    }
    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    assert_eq!(
        module
            .functions()
            .iter()
            .filter(|function| function.role() == tachyon_bytecode::FunctionRole::Method)
            .count(),
        3
    );
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("CreateAccessorPair").count(), 1);
    assert_eq!(entry.matches("AttachInstanceFields").count(), 1);
}

#[test]
/// Keeps static private elements on the constructor without allocating an instance plan.
fn compiler_freezes_static_private_elements() {
    let source = source(
        MediaType::JavaScript,
        "class C { static #field = 1; static #method() { return this.#field; } static get #value() { return this.#method(); } static set #value(next) { this.#field = next; } static read() { return this.#value; } } C.read();",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    let class = hir
        .statements()
        .iter()
        .find_map(|statement| match &statement.kind {
            HirStatementKind::VariableDeclaration(declaration) => declaration
                .declarators
                .iter()
                .find_map(|declarator| match declarator.initializer.as_ref() {
                    Some(HirExpression {
                        kind: HirExpressionKind::Class(class),
                        ..
                    }) => Some(class),
                    _ => None,
                }),
            _ => None,
        })
        .expect("class declaration retains one HIR class expression");
    assert!(
        class.elements.iter().any(
            |element| matches!(element, HirClassElement::PrivateField(field) if field.is_static)
        )
    );
    assert!(class.elements.iter().any(
        |element| matches!(element, HirClassElement::PrivateMethod(method) if method.is_static)
    ));
    assert!(class.elements.iter().any(
        |element| matches!(element, HirClassElement::PrivateAccessor(accessor) if accessor.is_static)
    ));

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let entry = tachyon_bytecode::disassemble(&module.functions()[0]).unwrap();
    assert_eq!(entry.matches("DefinePrivateField").count(), 1);
    assert_eq!(entry.matches("DefinePrivateMethod").count(), 1);
    assert_eq!(entry.matches("DefinePrivateAccessor").count(), 1);
    assert!(!entry.contains("AttachInstanceFields"));
}

#[test]
/// Freezes private brand checks without entering the public property-key or HasProperty path.
fn compiler_freezes_private_brand_check() {
    let source = source(
        MediaType::JavaScript,
        "class C { #value; static has(candidate) { return #value in candidate; } } C.has(new C());",
    );
    let hir = Compiler
        .lower_to_hir(source.clone(), CompileOptions::default())
        .unwrap();
    assert!(hir.functions().iter().any(|function| {
        function.body.iter().any(|statement| {
            matches!(
                &statement.kind,
                HirStatementKind::Return(Some(HirExpression {
                    kind: HirExpressionKind::PrivateIn { .. },
                    ..
                }))
            )
        })
    }));

    let module = Compiler.compile(source, CompileOptions::default()).unwrap();
    let function = module
        .functions()
        .iter()
        .map(|function| tachyon_bytecode::disassemble(function).unwrap())
        .find(|function| function.contains("HasPrivate"))
        .expect("private brand check owns a dedicated function body");
    assert!(!function.contains("HasProperty"));
    assert!(!function.contains("ToPropertyKey"));
}

proptest! {
    #[test]
    fn arbitrary_utf8_input_never_escapes_the_frontend(
        characters in proptest::collection::vec(any::<char>(), 0..128),
    ) {
        let text: String = characters.into_iter().collect();
        let _ = Compiler.parse(
            source(MediaType::JavaScript, &text),
            CompileOptions::default(),
        );
    }
}
