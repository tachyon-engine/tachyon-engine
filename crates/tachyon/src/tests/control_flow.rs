use super::*;

#[test]
/// Covers both script completion and ordinary-function loop control paths.
fn classic_for_loop_runs_update_on_continue_and_exits_on_break() {
    assert_eq!(
        execute_source(
            38,
            "let sum = 0; for (let index = 0; index < 4; index++) { if (index === 2) { continue; } sum += index; if (index === 3) { break; } } sum;",
        )
        .as_i32(),
        Some(4)
    );
    assert_eq!(
        execute_source(
            39,
            "function sumTo(limit) { let sum = 0; for (let index = 0; index < limit; ++index) { sum += index; } return sum; } sumTo(5);",
        )
        .as_i32(),
        Some(10)
    );
}

#[test]
/// Covers own/prototype keys, non-enumerable shadowing, and both loop-head forms.
fn for_in_enumerates_visible_string_keys() {
    assert_eq!(
        execute_source(
            67,
            "let score = 0; for (let key in { first: 1, second: 2 }) { if (key === 'first') score += 1; if (key === 'second') score += 10; } score;",
        )
        .as_i32(),
        Some(11)
    );
    assert_eq!(
        execute_source(
            68,
            "let proto = { inherited: 1, shadowed: 2 }; let object = Object.create(proto); object.own = 3; Object.defineProperty(object, 'shadowed', { enumerable: false }); let score = 0; for (let key in object) { if (key === 'own') score += 1; if (key === 'inherited') score += 10; if (key === 'shadowed') score += 100; } score;",
        )
        .as_i32(),
        Some(11)
    );
    assert_eq!(
        execute_source(
            69,
            "var key; for (key in { declared: 1 }) {} key === 'declared';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            73,
            "let holder = { value: '' }; for (holder.value in { member: 1 }) {} holder.value === 'member';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            74,
            "var proto = { prop: 'enumerable' }; var Construct = function () {}; Construct.prototype = proto; var child = new Construct(); Object.defineProperty(child, 'prop', { value: 'hidden', enumerable: false }); var accessed = false; for (var key in child) { if (key === 'prop') accessed = true; } accessed;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::False)
    );
    assert_eq!(
        execute_source(
            173,
            "let object = {}; object[9] = 9; object[2] = 2; object[10] = 10; object.tail = 1; let order = ''; for (let key in object) order += key + ','; order === '2,9,10,tail,';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Accessor keys enumerate from metadata without invoking getters or losing tombstone shadowing.
fn accessor_key_enumeration_is_value_independent() {
    assert_eq!(
        execute_source(
            180,
            "let calls = 0; let proto = {}; Object.defineProperty(proto, 'shadowed', { get() { calls++; return 1; }, enumerable: true, configurable: true }); let object = Object.create(proto); Object.defineProperty(object, 'shadowed', { get() { calls++; return 2; }, enumerable: false, configurable: true }); let before = ''; for (let key in object) before += key; delete object.shadowed; let after = ''; for (let key in object) after += key; before === '' && after === 'shadowed' && calls === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            181,
            "let calls = 0; let object = {}; Object.defineProperty(object, 'visible', { get() { calls++; return 1; }, enumerable: true }); Object.defineProperty(object, 'hidden', { get() { calls++; return 2; }, enumerable: false }); Object.keys(object); calls === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            182,
            "let calls = 0; let object = {}; Object.defineProperty(object, 'visible', { get() { calls++; return 1; }, enumerable: true }); Object.defineProperty(object, 'hidden', { get() { calls++; return 2; }, enumerable: false }); Object.getOwnPropertyNames(object); calls === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Exercises continue/break destinations plus nullish and string primitive enumeration.
fn for_in_preserves_control_flow_and_primitive_boundaries() {
    assert_eq!(
        execute_source(
            70,
            "let score = 0; for (let key in { first: 1, skip: 2, stop: 3, after: 4 }) { if (key === 'skip') continue; if (key === 'stop') break; score += 1; } score;",
        )
        .as_i32(),
        Some(1)
    );
    assert_eq!(
        execute_source(
            71,
            "let count = 0; for (let key in null) count++; for (let key in undefined) count++; for (let key in 'ab') { if (key === '0') count += 1; if (key === '1') count += 10; } count;",
        )
        .as_i32(),
        Some(11)
    );
    assert_eq!(
        execute_source(
            72,
            "function count(object) { let result = 0; for (const key in object) { result += 1; } return result; } count({ first: 1, second: 2 });",
        )
        .as_i32(),
        Some(2)
    );
}

#[test]
/// Covers pre-test/post-test ordering, continue targets, breaks, and script completion values.
fn while_and_do_while_preserve_loop_control_and_completion() {
    assert_eq!(
        execute_source(
            55,
            "let sum = 0; let index = 0; while (index < 5) { index++; if (index === 2) continue; sum += index; if (index === 4) break; } sum;",
        )
        .as_i32(),
        Some(8)
    );
    assert_eq!(
        execute_source(56, "do { 42; break; } while (true);").as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            57,
            "function total() { let index = 0; let sum = 0; do { index++; if (index < 3) continue; sum += index; } while (index < 4); return sum; } total();",
        )
        .as_i32(),
        Some(7)
    );
    assert_eq!(
        execute_source(58, "while (false) { 1; }").as_immediate(),
        Some(tachyon_value::Immediate::Undefined)
    );
}
