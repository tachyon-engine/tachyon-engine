use super::*;

#[test]
/// Exercises object literal creation, ordered data-property publication, and shape-backed updates.
fn object_literals_publish_and_update_plain_data_properties() {
    assert_eq!(
        execute_source(
            59,
            "let object = { answer: 40, label: 'ok' }; object.answer + 2;",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            60,
            "let object = { answer: 1 }; object.answer += 1; object.answer;",
        )
        .as_i32(),
        Some(2)
    );
    assert_eq!(
        execute_source(
            154,
            "let object = { 0: 'zero', 0x10: 'hex', 1e2: 'exp' }; object[0] === 'zero' && object[16] === 'hex' && object[100] === 'exp';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Checks computed string keys evaluate before values and use the same ordinary property storage.
fn object_literals_support_computed_string_keys() {
    assert_eq!(
        execute_source(
            61,
            "let key = 'answer'; let object = { [key]: 40 }; object.answer + 2;",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            62,
            "let calls = 0; let object = { [++calls]: 41 }; object[1] + calls;",
        )
        .as_i32(),
        Some(42)
    );
}

#[test]
/// Covers ordinary and computed object methods through the existing receiver call path.
fn object_literals_support_methods() {
    assert_eq!(
        execute_source(
            80,
            "let object = { answer() { return 40; } }; object.answer() + 2;"
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            81,
            "let object = { ['answer']() { return 40; } }; object.answer() + 2;",
        )
        .as_i32(),
        Some(42)
    );
}

#[test]
/// Ensures void evaluates its operand for side effects and always completes undefined.
fn void_evaluates_operand_and_returns_undefined() {
    assert_eq!(
        execute_source(82, "let calls = 0; void (++calls); calls;").as_i32(),
        Some(1)
    );
    assert!(execute_source(83, "void 42;").as_immediate().is_some());
}

#[test]
/// Covers primitive and callback-driven numeric conversion for unary plus.
fn unary_plus_converts_supported_primitives() {
    assert_eq!(execute_source(84, "+1;").as_i32(), Some(1));
    assert_eq!(execute_source(85, "+true;").as_i32(), Some(1));
    assert_eq!(execute_source(86, "+null;").as_i32(), Some(0));
    assert!(
        execute_source(87, "+undefined;")
            .as_f64()
            .is_some_and(f64::is_nan)
    );
    assert_eq!(execute_source(88, "+'0x10';").as_f64(), Some(16.0));
    assert_eq!(execute_source(89, "+'  1.5  ';").as_f64(), Some(1.5));
    assert_eq!(
        execute_source(
            177,
            "let order = 0; let direct = { valueOf() { order = order * 10 + 1; try { throw 8; } catch (error) { return error - 1; } }, toString() { order = 99; return 'wrong'; } }; let fallback = { valueOf() { order = order * 10 + 2; return {}; }, toString() { order = order * 10 + 3; return '8'; } }; let threw = false; try { +{ valueOf() { throw 42; } }; } catch (error) { threw = error === 42; } +direct === 7 && +fallback === 8 && order === 123 && threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Ensures unary negation shares primitive ToNumber conversion and preserves signed zero.
fn unary_minus_converts_supported_primitives() {
    assert_eq!(execute_source(90, "-'2.5';").as_f64(), Some(-2.5));
    assert_eq!(execute_source(91, "-true;").as_i32(), Some(-1));
    assert_eq!(execute_source(92, "-null;").as_f64(), Some(-0.0));
    assert_eq!(
        execute_source(
            178,
            "let order = 0; let direct = { valueOf() { order = order * 10 + 1; return 7; }, toString() { order = 99; return 'wrong'; } }; let fallback = { valueOf() { order = order * 10 + 2; return {}; }, toString() { order = order * 10 + 3; return '8'; } }; let threw = false; try { -{ valueOf() { throw 42; } }; } catch (error) { threw = error === 42; } -direct === -7 && -fallback === -8 && order === 123 && threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers ToNumber plus ECMAScript ToInt32 wrapping for bitwise complement.
fn bitwise_not_converts_supported_primitives() {
    assert_eq!(execute_source(93, "~1;").as_i32(), Some(-2));
    assert_eq!(execute_source(94, "~'1';").as_i32(), Some(-2));
    assert_eq!(execute_source(95, "~null;").as_i32(), Some(-1));
    assert_eq!(
        execute_source(
            179,
            "let order = 0; let direct = { valueOf() { order = order * 10 + 1; return 1; }, toString() { order = 99; return 'wrong'; } }; let fallback = { valueOf() { order = order * 10 + 2; return {}; }, toString() { order = order * 10 + 3; return '2'; } }; let threw = false; try { ~{ valueOf() { throw 42; } }; } catch (error) { threw = error === 42; } ~direct === -2 && ~fallback === -3 && order === 123 && threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers Add default-hint conversion, string concatenation, ordering, and abrupt completion.
fn addition_converts_primitives_and_objects_in_spec_order() {
    assert_eq!(
        execute_source(
            184,
            "'a' + 1 === 'a1' && 1 + 'a' === '1a' && null + 1 === 1 && true + false === 1 && 'x' + undefined === 'xundefined';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            185,
            "let order = 0; let right = { valueOf() { order = order * 10 + 2; return 2; } }; let left = { valueOf() { order = order * 10 + 1; right.valueOf = function() { order = order * 10 + 3; return 3; }; return 'x'; } }; let stopped = false; let rightCalls = 0; try { ({ valueOf() { throw 42; } }) + ({ valueOf() { rightCalls = rightCalls + 1; return 2; } }); } catch (error) { stopped = error === 42; } left + right === 'x3' && order === 13 && stopped && rightCalls === 0 && ({ valueOf() { return {}; }, toString() { return 'a'; } }) + 1 === 'a1';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            186,
            "let threw = false; try { '' + Symbol('value'); } catch (error) { threw = error instanceof TypeError; } threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers exact exotic hints, receiver identity, fallback, short-circuiting, and TypeError paths.
fn symbol_to_primitive_observes_the_exotic_conversion_contract() {
    assert_eq!(
        execute_source(
            189,
            "let seen = ''; let calls = 0; let object = { [Symbol.toPrimitive](hint) { seen = hint; calls++; return this === object && arguments.length === 1 ? 41 : 0; }, valueOf() { calls = 99; return 0; }, toString() { calls = 99; return 'bad'; } }; object + 1 === 42 && seen === 'default' && calls === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            190,
            "let hints = ''; let numeric = { [Symbol.toPrimitive](hint) { hints += hint; return 7; } }; let stringy = { [Symbol.toPrimitive](hint) { hints += hint; return 'ok'; } }; +numeric === 7 && String(stringy) === 'ok' && hints === 'numberstring';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            191,
            "let proto = { [Symbol.toPrimitive](hint) { return this === object && hint === 'default' ? 41 : 0; } }; let object = Object.create(proto); let absent = { valueOf() { return 20; } }; let undef = { [Symbol.toPrimitive]: undefined, valueOf() { return 21; } }; let nil = { [Symbol.toPrimitive]: null, valueOf() { return 22; } }; object + 1 === 42 && absent + undef + nil === 63;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            192,
            "let nonCallable = false; let objectResult = false; let fallbackCalls = 0; try { +({ [Symbol.toPrimitive]: 0 }); } catch (error) { nonCallable = error instanceof TypeError; } try { +({ [Symbol.toPrimitive]() { return {}; }, valueOf() { fallbackCalls++; return 1; } }); } catch (error) { objectResult = error instanceof TypeError; } nonCallable && objectResult && fallbackCalls === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Preserves abrupt completion and observes right-hand mutation only after left conversion finishes.
fn symbol_to_primitive_preserves_abrupt_and_left_to_right_order() {
    assert_eq!(
        execute_source(
            193,
            "let getterStopped = false; let getterRightCalls = 0; let getterObject = {}; Object.defineProperty(getterObject, Symbol.toPrimitive, { get() { throw 41; } }); try { getterObject + ({ [Symbol.toPrimitive]() { getterRightCalls++; return 1; } }); } catch (error) { getterStopped = error === 41; } let methodStopped = false; let methodRightCalls = 0; try { ({ [Symbol.toPrimitive]() { throw 42; } }) + ({ [Symbol.toPrimitive]() { methodRightCalls++; return 1; } }); } catch (error) { methodStopped = error === 42; } let resumed = ({ [Symbol.toPrimitive]() { return 1; } }) + 1; getterStopped && getterRightCalls === 0 && methodStopped && methodRightCalls === 0 && resumed === 2;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            194,
            "let order = 0; let right = { [Symbol.toPrimitive]() { order = order * 10 + 2; return 2; } }; let left = { [Symbol.toPrimitive]() { order = order * 10 + 1; right[Symbol.toPrimitive] = function() { order = order * 10 + 3; return 3; }; return 1; } }; left + right === 4 && order === 13;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Preserves left-to-right object conversion, mutation visibility, and abrupt completion.
fn numeric_binary_objects_resume_in_spec_order() {
    assert_eq!(
        execute_source(
            180,
            "let order = 0; let right = { valueOf() { order = order * 10 + 2; return 2; } }; let left = { valueOf() { order = order * 10 + 1; right.valueOf = function() { order = order * 10 + 3; return 3; }; return 8; } }; let difference = left - right; let rightCalls = 0; let stopped = false; try { ({ valueOf() { throw 42; } }) * ({ valueOf() { rightCalls = rightCalls + 1; return 2; } }); } catch (error) { stopped = error === 42; } difference === 5 && order === 13 && stopped && rightCalls === 0 && ({ valueOf() { return 8; } }) / ({ valueOf() { return 2; } }) === 4;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers numeric/string coercion and integer results for all bitwise binary operators.
fn bitwise_binary_converts_supported_primitives() {
    assert_eq!(execute_source(96, "5 & 3;").as_i32(), Some(1));
    assert_eq!(execute_source(97, "'5' | 2;").as_i32(), Some(7));
    assert_eq!(execute_source(98, "5 ^ 3;").as_i32(), Some(6));
    assert_eq!(
        execute_source(
            181,
            "(({ valueOf() { return 5; } }) & ({ valueOf() { return 3; } })) === 1 && (({ valueOf() { return 5; } }) | ({ valueOf() { return 2; } })) === 7 && (({ valueOf() { return 5; } }) ^ ({ valueOf() { return 3; } })) === 6;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers signed and unsigned shift counts and ToNumber coercion.
fn shifts_convert_supported_primitives() {
    assert_eq!(execute_source(99, "5 << 1;").as_i32(), Some(10));
    assert_eq!(execute_source(100, "'8' >> 1;").as_i32(), Some(4));
    assert_eq!(execute_source(101, "-1 >>> 30;").as_f64(), Some(3.0));
    assert_eq!(
        execute_source(
            182,
            "(({ valueOf() { return 5; } }) << ({ valueOf() { return 1; } })) === 10 && (({ valueOf() { return 8; } }) >> ({ valueOf() { return 1; } })) === 4 && (({ valueOf() { return -1; } }) >>> ({ valueOf() { return 30; } })) === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers numeric coercion for remainder and exponentiation.
fn remainder_and_exponentiation_convert_supported_primitives() {
    assert_eq!(execute_source(102, "'5' % 2;").as_f64(), Some(1.0));
    assert_eq!(execute_source(103, "2 ** '3';").as_f64(), Some(8.0));
    assert_eq!(
        execute_source(
            183,
            "(({ valueOf() { return 5; } }) % ({ valueOf() { return 2; } })) === 1 && (({ valueOf() { return 2; } }) ** ({ valueOf() { return 3; } })) === 8;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers numeric/string relational semantics and callback-driven object conversion order.
fn relational_operators_compare_supported_primitives() {
    assert_eq!(
        execute_source(104, "3 > 2;").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(105, "2 <= '2';").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(106, "3 >= 4;").as_immediate(),
        Some(tachyon_value::Immediate::False)
    );
    assert_eq!(
        execute_source(
            187,
            "!('2' < '10') && '2' > '10' && 'a' <= 'a' && 'é' < 'Ā' && '2' < 10 && 2 <= '2';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            188,
            "let order = 0; let right = { valueOf() { order = order * 10 + 2; return 2; } }; let left = { valueOf() { order = order * 10 + 1; right.valueOf = function() { order = order * 10 + 3; return 3; }; return 4; } }; let greater = left > right; let firstOrder = order; order = 0; let lessEqual = left <= right; let secondOrder = order; let rightCalls = 0; let stopped = false; try { ({ valueOf() { throw 42; } }) >= ({ valueOf() { rightCalls = rightCalls + 1; return 2; } }); } catch (error) { stopped = error === 42; } greater && !lessEqual && firstOrder === 13 && secondOrder === 13 && stopped && rightCalls === 0 && ({ valueOf() { return {}; }, toString() { return 'a'; } }) < ({ valueOf() { return 'b'; } });",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers abstract equality coercion without changing strict equality behavior.
fn loose_equality_converts_supported_primitives() {
    assert_eq!(
        execute_source(107, "1 == '1';").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(108, "null == undefined;").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(109, "false == 0;").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(110, "1 != '2';").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(163, "'0' != 0;").as_immediate(),
        Some(tachyon_value::Immediate::False)
    );
    assert_eq!(
        execute_source(164, "'0' !== 0;").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            197,
            "let symbol = Symbol('value'); !(1 == symbol) && !(true == symbol) && symbol != 1 && symbol != false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            195,
            "let calls = 0; let object = { [Symbol.toPrimitive](hint) { calls++; return hint === 'default' && this === object ? 1 : 0; } }; let same = {}; let left = object == true; let right = true == object; let nullish = object == null || undefined == object; let objects = same == same && !(same == {}); left && right && !nullish && objects && calls === 2;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            196,
            "let order = 0; let ordinary = { valueOf() { order = order * 10 + 1; return {}; }, toString() { order = order * 10 + 2; return '7'; } }; let abrupt = false; try { 1 != ({ [Symbol.toPrimitive]() { throw 42; } }); } catch (error) { abrupt = error === 42; } let objectResult = false; try { 1 == ({ [Symbol.toPrimitive]() { return {}; } }); } catch (error) { objectResult = error instanceof TypeError; } ordinary == 7 && 7 == ordinary && order === 1212 && abrupt && objectResult;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers own and inherited ordinary data-property membership checks.
fn in_operator_checks_supported_objects() {
    assert_eq!(
        execute_source(111, "'answer' in { answer: 42 };").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(112, "'missing' in { answer: 42 };").as_immediate(),
        Some(tachyon_value::Immediate::False)
    );
}

#[test]
/// Ensures an unresolved global name is safe inside typeof and reports undefined.
fn typeof_unresolved_global_is_undefined() {
    assert!(
        execute_source(113, "typeof definitely_missing_name;")
            .as_heap_ref()
            .is_some(),
    );
}

#[test]
/// Covers deleting ordinary own properties and observing the deleted slot as absent.
fn delete_removes_supported_object_properties() {
    assert!(matches!(
        execute_source(
            114,
            "let object = { answer: 42 }; delete object.answer; !('answer' in object);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    ));
}
