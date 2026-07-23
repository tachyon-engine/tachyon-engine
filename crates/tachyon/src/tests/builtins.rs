use super::*;

#[test]
/// Calls Array forEach with stable value/index/receiver arguments and ignores holes.
fn array_for_each_resumes_callbacks_in_index_order() {
    let value = execute_source(
        1_039,
        "var trace = 0; var array = [1, 2, 3]; delete array[1]; array.forEach(function(value, index, receiver) { trace = trace * 10 + value + index; if (receiver !== array) trace = -1; }); trace;",
    );
    assert_eq!(
        value.as_f64(),
        Some(15.0),
        "unexpected forEach trace: {value:?}"
    );
    assert_eq!(
        execute_source(
            1_040,
            "function checkSequence(array) { var ok = true; array.forEach(function(value, index) { if (value !== index + 1) ok = false; }); return ok; } checkSequence([1, 2, 3, 4, 5, 6, 7, 8]);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Keeps an outer Promise reaction continuation below nested Array callback frames.
fn array_for_each_does_not_consume_outer_promise_continuation() {
    let compile = |source_id, text: &'static str| {
        Compiler
            .compile(
                SourceText::new(
                    SourceId::new(source_id),
                    SourceName::new("array-foreach-promise-boundary"),
                    MediaType::JavaScript,
                    Arc::from(text),
                ),
                CompileOptions::default(),
            )
            .unwrap()
    };
    let setup = compile(
        1_041,
        "var trace = 0; Promise.resolve([1, 2]).then(function(values) { values.forEach(function(value) { trace = trace * 10 + value; }); trace = trace * 10 + 3; }).then(function() { trace = trace * 10 + 4; });",
    );
    let probe = compile(1_042, "trace;");
    let mut isolate = test_isolate();
    assert!(matches!(
        isolate.execute(
            &setup,
            ExecutionBudget {
                fuel: 512,
                quantum: 512,
            },
        ),
        Ok(RunOutcome::Completed(_))
    ));
    assert!(matches!(
        isolate.execute(
            &probe,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        ),
        Ok(RunOutcome::Completed(value)) if value.as_i32() == Some(1_234)
    ));
}

#[test]
/// Drains standard Array iterables through the Map and Set constructor protocol.
fn collection_constructors_consume_array_iterables() {
    assert_eq!(
        execute_source(
            1_017,
            "var map = new Map([[1, 2], [3, 4]]); var set = new Set([5, 6, 5]); map.get(1) === 2 && map.get(3) === 4 && set.size === 2 && set.has(6);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Preserves the cached iterator and adder call boundaries across bytecode callback frames.
fn collection_constructors_resume_custom_iterators_and_adders() {
    assert_eq!(
        execute_source(
            1_018,
            "let calls = 0; let source = { [Symbol.iterator]() { let index = 0; return { next() { index += 1; return index === 1 ? { done: false, value: [7, 9] } : { done: true }; } }; } }; let original = Map.prototype.set; Map.prototype.set = function(key, value) { calls += 1; return original.call(this, key, value); }; let map = new Map(source); map.get(7) === 9 && calls === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Keeps collection forEach scans live across callback frames and collection mutations.
fn collection_for_each_uses_live_insertion_order_and_callback_contract() {
    assert_eq!(
        execute_source(
            1_019,
            "var map = new Map([[1, 10], [2, 20]]); var mapSeen = ''; var holder = { tag: 7 }; map.forEach(function(value, key, received) { mapSeen = mapSeen + key + value + this.tag + (received === map ? 1 : 0); if (key === 1) { map.delete(2); map.set(3, 30); } }, holder); var set = new Set([4, 5]); var setSeen = ''; set.forEach(function(value, entry, received) { setSeen = setSeen + value + entry + (received === set ? 1 : 0); if (value === 4) { set.delete(5); set.add(6); } }); mapSeen === '1107133071' && setSeen === '441661';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Preserves an existing Map entry and inserts a missing key's supplied default exactly once.
fn map_get_or_insert_uses_same_value_zero_lookup() {
    assert_eq!(
        execute_source(
            1_021,
            "var map = new Map(); map.set(1, 2); map.getOrInsert(1, 9) === 2 && map.getOrInsert(3, 4) === 4 && map.get(3) === 4 && map.size === 2;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Publishes `%String.prototype%` and routes primitive string length through its exotic boundary.
fn primitive_string_length_and_prototype_are_available() {
    assert_eq!(
        execute_source(
            924,
            "String.prototype.constructor === String && 'tachyon'.length === 7 && String('x').length === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            925,
            "let value = 'x'; value.length = 9; value.length === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Reads only canonical primitive-string indices and preserves UTF-16 code-unit boundaries.
fn primitive_string_indexed_properties_follow_string_exotic_rules() {
    assert_eq!(
        execute_source(
            926,
            "'tachyon'[1] === 'a' && 'tachyon'[99] === undefined && 'tachyon'['01'] === undefined;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Exercises RegExp construction, result metadata, and global-state advancement end to end.
fn regexp_constructor_exec_and_global_last_index_are_available() {
    assert_eq!(
        execute_source(
            947,
            "let r = new RegExp('a', 'g'); let first = r.exec('baab'); let second = r.exec('baab'); first[0] === 'a' && first.index === 1 && first.input === 'baab' && second.index === 2 && r.lastIndex === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Distinguishes RegExp's absent-argument semantics from ordinary String conversion.
fn regexp_constructor_defaults_to_the_canonical_empty_pattern() {
    assert_eq!(
        execute_source(
            950,
            "let omitted = RegExp(); let undefinedPattern = RegExp(undefined); let empty = RegExp(''); omitted.source === '(?:)' && undefinedPattern.source === '(?:)' && empty.source === '(?:)' && omitted.test('x') && undefinedPattern.test('x');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Materializes matched and unmatched positional captures in RegExp exec results.
fn regexp_exec_preserves_capture_slots() {
    assert_eq!(
        execute_source(
            951,
            "let result = /(a)(b)?/.exec('a'); result.length === 3 && result[0] === 'a' && result[1] === 'a' && result[2] === undefined;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Maps backend named-capture ranges into the RegExp result groups object.
fn regexp_exec_materializes_named_groups() {
    assert_eq!(
        execute_source(
            952,
            "let result = /(?<word>a)/.exec('a'); result.groups.word === 'a';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Accepts boxed String values through the same private StringData conversion as String methods.
fn regexp_accepts_string_wrapper_pattern_flags_and_input() {
    assert_eq!(
        execute_source(
            954,
            "let regexp = RegExp(new String('a'), new String('g')); regexp.test(new String('a')) && regexp.lastIndex === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Formats the stored source and canonical flag string through RegExp.prototype.toString.
fn regexp_to_string_uses_source_and_flags() {
    assert_eq!(
        execute_source(955, "new RegExp('a', 'gi').toString() === '/a/gi';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Copies source and flags from a RegExp pattern when no overriding flags are supplied.
fn regexp_constructor_copies_existing_regexp_pattern() {
    assert_eq!(
        execute_source(956, "let source = /a/gi; let copy = new RegExp(source); copy !== source && copy.toString() === '/a/gi';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Keeps sticky matching anchored at `lastIndex` and resets state on a failed execution.
fn regexp_test_observes_sticky_last_index_and_failure_reset() {
    assert_eq!(
        execute_source(
            948,
            "let r = new RegExp('a', 'y'); r.lastIndex = 1; r.test('ba') && r.lastIndex === 2 && !r.test('ba') && r.lastIndex === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Ensures each RegExp literal evaluation creates an independent `lastIndex` state carrier.
fn regexp_literals_lower_to_independent_runtime_objects() {
    assert_eq!(
        execute_source(
            949,
            "let first = /a/g; let second = /a/g; first.test('aa') && first.lastIndex === 1 && second.lastIndex === 0 && second.test('aa') && second.lastIndex === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Exercises String character methods against primitive UTF-16 code units and out-of-range values.
fn string_character_methods_preserve_utf16_and_numeric_boundaries() {
    assert_eq!(
        execute_source(
            927,
            "'abc'.charAt() === 'a' && 'abc'.charAt(1) === 'b' && 'abc'.charAt(9) === '' && 'abc'.charCodeAt(1) === 98 && Number.isNaN('abc'.charCodeAt(9));",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Verifies String.prototype.slice normalizes relative positions without splitting UTF-16 units.
fn string_slice_uses_relative_utf16_code_unit_indices() {
    assert_eq!(
        execute_source(
            928,
            "'tachyon'.slice(1, -1) === 'achyo' && 'tachyon'.slice(-3) === 'yon' && 'tachyon'.slice(5, 2) === '' && 'tachyon'.slice(NaN, 1) === 't';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Verifies String.prototype.substring clamps negatives and swaps reversed endpoints.
fn string_substring_clamps_and_reorders_positions() {
    assert_eq!(
        execute_source(
            929,
            "'tachyon'.substring(1, -1) === 't' && 'tachyon'.substring(5, 2) === 'chy' && 'tachyon'.substring(NaN, 2) === 'ta';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Searches primitive String values in UTF-16 code-unit order with exact empty-needle behavior.
fn string_index_of_normalizes_positions_and_empty_needles() {
    assert_eq!(
        execute_source(
            930,
            "'tachyon'.indexOf('ch') === 2 && 'tachyon'.indexOf('t', 1) === -1 && 'tachyon'.indexOf('', 99) === 7 && 'tachyon'.indexOf('ta', -1) === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Confirms String.prototype.includes reuses indexOf's code-unit and position semantics.
fn string_includes_reuses_utf16_search_boundaries() {
    assert_eq!(
        execute_source(
            931,
            "'tachyon'.includes('chy') && !'tachyon'.includes('ta', 1) && 'tachyon'.includes('', 99);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Trims the ECMAScript whitespace set and preserves the start/end alias identities.
fn string_trim_methods_use_ecmascript_code_units() {
    assert_eq!(
        execute_source(
            932,
            "'\\u00a0\\t tachyon \\u2029'.trim() === 'tachyon' && '  x  '.trimStart() === 'x  ' && '  x  '.trimEnd() === '  x' && String.prototype.trimLeft === String.prototype.trimStart && String.prototype.trimRight === String.prototype.trimEnd;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers the UTF-16 search and allocation-free capacity paths for the remaining String core.
fn string_search_and_construction_methods_cover_code_unit_boundaries() {
    assert_eq!(
        execute_source(
            933,
            "'abca'.lastIndexOf('a') === 3 && 'abca'.lastIndexOf('a', 2) === 0 && 'tachyon'.startsWith('ch', 2) && 'tachyon'.endsWith('yon') && !'tachyon'.endsWith('yon', 6) && 'a'.concat('b', 3) === 'ab3' && 'ab'.repeat(3) === 'ababab' && 'x'.padStart(4, 'ab') === 'abax' && 'x'.padEnd(4, 'ab') === 'xaba';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers code-unit and code-point String APIs, including surrogate-pair materialization.
fn string_code_point_methods_preserve_utf16_boundaries() {
    assert_eq!(
        execute_source(
            934,
            r#"'abc'.at(-1) === 'c' && 'abc'.at(3) === undefined && '\uD83D\uDE00'.codePointAt(0) === 128512 && String.fromCharCode(65, 0xD83D, 0xDE00) === 'A\uD83D\uDE00' && String.fromCodePoint(0x1F600) === '\uD83D\uDE00';"#,
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Publishes String's primitive identity methods without coercing unrelated receivers.
fn string_identity_methods_return_the_primitive_receiver() {
    assert_eq!(
        execute_source(
            935,
            "'tachyon'.toString() === 'tachyon' && String.prototype.valueOf.call('tachyon') === 'tachyon';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Preserves valid UTF-16 pairs while replacing exactly the unpaired surrogate code units.
fn string_well_formed_methods_handle_unpaired_surrogates() {
    assert_eq!(
        execute_source(
            936,
            "String.fromCharCode(0xD800, 120, 0xDC00).isWellFormed() === false;"
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            936,
            "let malformed = String.fromCharCode(0xD800, 120, 0xDC00); malformed.isWellFormed() === false && String.fromCodePoint(0x1F600).isWellFormed() && malformed.toWellFormed() === String.fromCharCode(0xFFFD, 120, 0xFFFD);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Verifies that String construction creates a branded ordinary wrapper with its prototype chain.
fn string_constructor_creates_a_wrapper_object() {
    assert_eq!(
        execute_source(
            937,
            "let value = new String('tachyon'); value instanceof String && value.toString() === 'tachyon' && value.valueOf() === 'tachyon';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Routes String wrapper length through its retained UTF-16 primitive data.
fn string_wrapper_length_uses_string_exotic_data() {
    assert_eq!(
        execute_source(938, "new String('tachyon').length === 7;").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Reuses primitive UTF-16 builtins after extracting a String wrapper's private data.
fn string_wrapper_methods_use_the_private_string_data() {
    assert_eq!(
        execute_source(
            939,
            "let value = new String('tachyon'); value.charAt(1) === 'a' && value.slice(1, 4) === 'ach' && value.includes('chy');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Reads canonical String exotic indices from a wrapper's private UTF-16 backing.
fn string_wrapper_indexed_properties_read_private_string_data() {
    assert_eq!(
        execute_source(940, "new String('tachyon')[2] === 'c';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers JSON namespace publication, nested UTF-16 materialization, duplicate keys, and syntax errors.
fn json_parse_materializes_engine_values_and_rejects_extensions() {
    assert_eq!(
        execute_source(
            920,
            "let value = JSON.parse('{\"a\":[1,{\"b\":true}],\"a\":4,\"escaped\":\"\\\\u0041\"}'); value.a === 4 && value.escaped === 'A';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            921,
            "let threw = false; try { JSON.parse('[1,]'); } catch (error) { threw = error instanceof SyntaxError; } threw && JSON.parse.length === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers synchronous JSON serialization of primitive, Array, ordinary-object, and cyclic values.
fn json_stringify_serializes_core_values_and_rejects_cycles() {
    assert_eq!(
        execute_source(
            922,
            "JSON.stringify({ b: [true, undefined, -0, NaN], a: 'x\\n' }) === '{\"b\":[true,null,0,null],\"a\":\"x\\\\n\"}';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            923,
            "let value = {}; value.self = value; let threw = false; try { JSON.stringify(value); } catch (error) { threw = error instanceof TypeError; } threw && JSON.stringify.length === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers dense array indexing, elision as an absent property, and length publication.
fn array_literals_support_basic_indexing_and_length() {
    assert_eq!(
        execute_source(115, "let array = [40, 2]; array[0] + array[1];").as_i32(),
        Some(42)
    );
    assert_eq!(execute_source(116, "[1, , 3].length;").as_i32(), Some(3));
    assert_eq!(
        execute_source(117, "[1, , 3] instanceof Array;").as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            118,
            "let value = []; value.toString === Array.prototype.toString;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(119, "let source = [2, 3]; [...source, 4][2] === 4;").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(120, "[0, ...[1, 2], 3].length;").as_i32(),
        Some(4)
    );
    assert_eq!(
        execute_source(121, "[...'ab'][1] === 'b';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies native Array call/construct paths, indexed storage, length, and concat flattening.
#[test]
fn native_array_constructor_and_concat_preserve_elements() {
    assert_eq!(
        execute_source(
            60,
            "let values = new Array('Saab', 'Volvo'); values.concat(['BMW'])[2] === 'BMW';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(61, "let values = Array('a', 'b'); values.length;").as_i32(),
        Some(2)
    );
    assert_eq!(execute_source(62, "new Array(3).length;").as_i32(), Some(3));
    assert_eq!(
        execute_source(64, "[1, 'two', null, true].toString() === '1,two,,true';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            63,
            "let item = { '0': 9, length: 1 }; let joined = [1].concat(2, item); joined[1] === 2 && joined[2] === item && joined.length === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Covers Array exotic identity, generic push/join, holes, and length attributes.
#[test]
fn array_identity_push_and_join_follow_array_like_semantics() {
    assert_eq!(
        execute_source(
            124,
            "Array.isArray([]) && Array.isArray(Array.prototype) && !Array.isArray({ length: 0 }) && !Array.isArray(Object.create(Array.prototype));",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            125,
            "let values = [1]; values.push(2, 3) === 3 && values.length === 3 && values[2] === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            128,
            "let values = [1]; values.push(2, 3); values.join('-') === '1-2-3';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            126,
            "let object = { length: 1 }; object[0] = 'a'; Array.prototype.push.call(object, 'b') === 2 && object.length === 2 && object[1] === 'b';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            129,
            "let object = { length: 2 }; object[0] = 'a'; object[1] = 'b'; Array.prototype.join.call(object, ':') === 'a:b';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            133,
            "let values = [10, 20, NaN, -0]; values.at(1) === 20 && values.at(-1) === 0 && values.at(99) === undefined && values.indexOf(NaN) === -1 && values.includes(NaN) && values.includes(0) && values.indexOf(0) === 3 && [,].includes(undefined);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            134,
            "let object = { length: 3 }; object[0] = 'a'; object[2] = 'c'; Array.prototype.at.call(object, -1) === 'c' && Array.prototype.indexOf.call(object, 'a') === 0 && !Array.prototype.includes.call(object, 'b');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            130,
            "let object = { length: 1 }; object[0] = 'a'; Array.prototype.push.call(object, 'b'); Array.prototype.join.call(object, ':') === 'a:b';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            140,
            "let values = [1, 2, 3]; values.pop() === 3 && values.length === 2 && values[1] === 2;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            141,
            "let values = [1, , 3, 4]; let copy = values.slice(1, 3); copy.length === 2 && copy[0] === undefined && copy[1] === 3 && values.length === 4;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            142,
            "let object = { length: 3 }; object[0] = 'a'; object[2] = 'c'; Array.prototype.slice.call(object, 0, 3).length === 3 && Array.prototype.pop.call(object) === 'c' && object.length === 2;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            143,
            "let values = [1, , 3]; values.shift() === 1 && values.length === 2 && values[0] === undefined && values[1] === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            144,
            "let values = [2, 3]; values.unshift(0, 1) === 4 && values.join('-') === '0-1-2-3';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            145,
            "let values = [1, , 3, 4]; values.reverse(); values.length === 4 && values[0] === 4 && values[1] === 3 && values[2] === undefined && values[3] === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            148,
            "let values = [1, 2, 3, 2]; values.lastIndexOf(2) === 3 && values.lastIndexOf(2, -2) === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            149,
            "let values = [1, , 3, 4]; values.fill(0, 1, -1) === values && values.join('-') === '1-0-0-4';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            150,
            "let values = [1, 2, 3, 4, 5]; values.copyWithin(1, 3, 5) === values && values.join('-') === '1-4-5-4-5';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            151,
            "let values = [1, , 3, 4]; values.copyWithin(0, 1, 3); values.length === 4 && values[0] === undefined && values[1] === 3 && values[2] === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            152,
            "let values = [1, [2, [3]], , 4]; let flat = values.flat(2); flat.length === 4 && flat[0] === 1 && flat[1] === 2 && flat[2] === 3 && flat[3] === 4;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            153,
            "let values = [1, [2, [3]]]; values.flat(1)[2][0] === 3 && values.flat(0)[1][1][0] === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            127,
            "let recursive = []; recursive.push(recursive); let descriptor = Object.getOwnPropertyDescriptor([], 'length'); [1, , null, undefined, 4].join('-') === '1----4' && recursive.join() === '' && descriptor.writable === true && descriptor.enumerable === false && descriptor.configurable === false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Runs Array.prototype.filter through callback frames and preserves selected order.
fn array_filter_runs_resumable_callback_iteration() {
    assert_eq!(
        execute_source(
            1_140,
            "var result = [1, 2, 3, 4].filter(function(value) { return value % 2 === 0; }); result.length === 2 && result[0] === 2 && result[1] === 4;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1_141),
                SourceName::new("sparse-filter-trampoline"),
                MediaType::JavaScript,
                Arc::from(
                    "var values = []; values.length = 20000; values[19999] = 7; var calls = 0; var result = values.filter(function(value) { calls += 1; return true; }); calls === 1 && result.length === 1 && result[0] === 7;",
                ),
            ),
            CompileOptions::default(),
        )
        .unwrap();
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(8 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024).with_max_shapes(384),
    ))
    .unwrap();
    assert!(matches!(
        isolate.execute(
            &module,
            ExecutionBudget {
                fuel: 512,
                quantum: 512,
            },
        ),
        Ok(RunOutcome::Completed(value))
            if value.as_immediate() == Some(tachyon_value::Immediate::True)
    ));
}

#[test]
/// Covers descriptor harness primitives used by multiple built-in test262 directories.
fn object_names_enumerability_and_math_pow_are_available() {
    assert_eq!(
        execute_source(
            131,
            "let descriptor = { value: 1 }; let names = Object.getOwnPropertyNames(descriptor); names.length === 1 && names[0] === 'value' && Object.prototype.propertyIsEnumerable.call(descriptor, 'value') && Math.pow(2, 5) === 32;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            132,
            "function verify(object, name, descriptor) { return arguments.length > 2 && Object.getOwnPropertyNames(descriptor).length === 4 && Object.getOwnPropertyDescriptor(object, name).value === descriptor.value && Object.prototype.propertyIsEnumerable.call(object, name) === false; } verify(Function.prototype.bind, 'length', { value: 1, writable: false, enumerable: false, configurable: true });",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            146,
            "let names = Object.getOwnPropertyNames(Function.prototype.bind); names.includes('length') && names.includes('name') && names.includes('prototype') === false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

/// Covers the mutation probes used by test262 propertyHelper for immutable constants.
#[test]
fn immutable_intrinsic_constants_survive_descriptor_harness_probes() {
    assert_eq!(
        execute_source(
            170,
            "let original = Number.EPSILON; Number.EPSILON = 'unlikelyValue'; Number.EPSILON === original && Number.EPSILON > 0 && Number.EPSILON < 0.000001;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            171,
            "'use strict'; let threw = false; try { Number.EPSILON = 'unlikelyValue'; } catch (error) { threw = error instanceof TypeError; } threw && Number.EPSILON > 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            172,
            "let name = 'EPSILON'; let removed = delete Number[name]; let seen = false; for (let key in Number) { if (key === name) seen = true; } removed === false && Number[name] > 0 && seen === false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            173,
            "'use strict'; let name = 'EPSILON'; let threw = false; try { delete Number[name]; } catch (error) { threw = error instanceof TypeError; } threw && Number[name] > 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies Object intrinsics use the ordinary shape path for own-property operations.
#[test]
fn object_define_property_and_has_own_property_work() {
    assert_eq!(
        execute_source(
            65,
            "let object = {}; Object.defineProperty(object, 'answer', { value: 42 }); object.hasOwnProperty('answer') && object.answer === 42;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(66, "Object('value').hasOwnProperty('value');").as_immediate(),
        Some(tachyon_value::Immediate::False),
    );
}

/// Verifies data descriptor defaults, reconfiguration, enumeration, and native metadata.
#[test]
fn data_property_descriptors_preserve_flags_and_values() {
    assert_eq!(
        execute_source(
            83,
            "let object = {}; Object.defineProperty(object, 'hidden', { value: 1 }); let descriptor = Object.getOwnPropertyDescriptor(object, 'hidden'); object.hidden = 2; descriptor.value === 1 && !descriptor.writable && !descriptor.enumerable && !descriptor.configurable && object.hidden === 1 && Object.keys(object).length === 0;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            84,
            "let object = {}; Object.defineProperty(object, 'first', { value: 1, writable: true, enumerable: true, configurable: true }); Object.defineProperty(object, 'second', { value: 2, enumerable: true }); Object.defineProperty(object, 'first', { value: 3, writable: false, enumerable: false, configurable: false }); let descriptor = Object.getOwnPropertyDescriptor(object, 'first'); Object.keys(object)[0] === 'second' && descriptor.value === 3 && !descriptor.writable && !descriptor.enumerable && !descriptor.configurable && delete object.first === false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            85,
            "let method = Object.preventExtensions; let metadata = Object.getOwnPropertyDescriptor(method, 'name'); let object = {}; Object.defineProperty(object, 'fixed', { value: 1 }); let threw = false; try { Object.defineProperty(object, 'fixed', { configurable: true }); } catch (error) { threw = error instanceof TypeError; } metadata.value === 'preventExtensions' && !metadata.writable && !metadata.enumerable && metadata.configurable && threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            86,
            "var object = { property: 1 }; var descriptor = Object.getOwnPropertyDescriptor(object, 'property'); if (!('writable' in descriptor)) { 0; } else if (!delete descriptor.writable) { 2; } else if ('writable' in descriptor) { 3; } else { 1; }",
        )
        .as_i32(),
        Some(1),
    );
}

/// Covers observable ToPropertyDescriptor getters and accessor-kind reflection end to end.
#[test]
fn accessor_property_descriptors_resume_and_reflect() {
    assert_eq!(
        execute_source(
            220,
            "let order = ''; let proto = {}; Object.defineProperty(proto, 'enumerable', { get() { order += 'e'; return true; } }); let desc = Object.create(proto); Object.defineProperty(desc, 'value', { get() { order += 'v'; return 42; } }); Object.defineProperty(desc, 'writable', { get() { order += 'w'; return true; } }); let target = {}; Object.defineProperty(target, 'answer', desc); let actual = Object.getOwnPropertyDescriptor(target, 'answer'); order === 'evw' && target.answer === 42 && actual.value === 42 && actual.writable && actual.enumerable && !actual.configurable;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            221,
            "let getter = function() { return this.answer; }; let setter = function(value) { this.answer = value; }; let object = { answer: 7 }; Object.defineProperty(object, 'computed', { get: getter, set: setter, enumerable: true, configurable: true }); let desc = Object.getOwnPropertyDescriptor(object, 'computed'); let before = object.hasOwnProperty('computed') && object.propertyIsEnumerable('computed') && desc.get === getter && desc.set === setter && desc.enumerable && desc.configurable && !('value' in desc) && !('writable' in desc); object.computed = 9; before && object.computed === 9;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Descriptor getter abrupt completions preserve identity and mixed descriptors fail after Get order.
#[test]
fn accessor_property_descriptor_abrupt_and_mixed_order() {
    assert_eq!(
        execute_source(
            222,
            "let marker = {}; let target = {}; let desc = {}; Object.defineProperty(desc, 'configurable', { get() { throw marker; } }); let caught = false; try { Object.defineProperty(target, 'x', desc); } catch (error) { caught = error === marker; } caught && !target.hasOwnProperty('x');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            223,
            "let order = ''; let desc = {}; Object.defineProperty(desc, 'value', { get() { order += 'v'; return 1; } }); Object.defineProperty(desc, 'get', { get() { order += 'g'; return function() {}; } }); Object.defineProperty(desc, 'set', { get() { order += 's'; return undefined; } }); let threw = false; try { Object.defineProperty({}, 'x', desc); } catch (error) { threw = error instanceof TypeError; } threw && order === 'vgs';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Exercises resumable object keys across each Object property-query and definition builtin.
fn object_builtin_property_keys_resume_once_with_string_hint() {
    assert_eq!(
        execute_source(
            202,
            "let calls = 0; let key = { [Symbol.toPrimitive](hint) { calls++; return hint === 'string' ? 'answer' : 'wrong'; } }; let object = { answer: 42 }; let descriptor = Object.getOwnPropertyDescriptor(object, key); let staticOwn = Object.hasOwn(object, key); let protoOwn = object.hasOwnProperty(key); let enumerable = object.propertyIsEnumerable(key); descriptor.value === 42 && staticOwn && protoOwn && enumerable && calls === 4;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            203,
            "let order = ''; let key = { [Symbol.toPrimitive](hint) { order += hint === 'string' ? 'k' : 'x'; return 'created'; } }; let descriptor = {}; Object.defineProperty(descriptor, 'value', { get() { order += 'd'; return 42; } }); let target = {}; Object.defineProperty(target, key, descriptor); order === 'kd' && target.created === 42;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Locks the opposite ToObject/ToPropertyKey order of static and prototype Object queries.
fn object_builtin_property_key_guards_follow_each_spec_order() {
    assert_eq!(
        execute_source(
            204,
            "let staticCalls = 0; let staticKey = { [Symbol.toPrimitive]() { staticCalls++; return 'x'; } }; let staticError = false; try { Object.hasOwn(null, staticKey); } catch (error) { staticError = error instanceof TypeError; } let prototypeCalls = 0; let prototypeKey = { [Symbol.toPrimitive]() { prototypeCalls++; throw 42; } }; let prototypeError = false; try { Object.prototype.hasOwnProperty.call(null, prototypeKey); } catch (error) { prototypeError = error === 42; } staticError && staticCalls === 0 && prototypeError && prototypeCalls === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies primitive constructors reuse the VM numeric, truthiness, and string contracts.
#[test]
fn primitive_constructors_convert_values() {
    assert_eq!(
        execute_source(67, "String(42) === '42' && String(null) === 'null';").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            175,
            "let order = 0; let direct = { toString() { order = order * 10 + 1; return 'direct'; }, valueOf() { order = 99; return 'wrong'; } }; let fallback = { toString() { order = order * 10 + 2; return {}; }, valueOf() { order = order * 10 + 3; try { throw 7; } catch (error) { return error - 1; } } }; let threw = false; try { String({ toString() { throw 42; } }); } catch (error) { threw = error === 42; } String(direct) === 'direct' && String(fallback) === '6' && order === 123 && threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(68, "Number('42') === 42;").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            176,
            "let calls = 0; let direct = { valueOf() { calls = calls + 1; return 7; }, toString() { return 'wrong'; } }; let fallback = { valueOf() { return {}; }, toString() { return '8'; } }; let called = Number(direct) === 7 && Number(fallback) === 8; let boxed = new Number(direct); let callThrow = false; let constructThrow = false; let abrupt = { valueOf() { throw 42; } }; try { Number(abrupt); } catch (error) { callThrow = error === 42; } try { new Number(abrupt); } catch (error) { constructThrow = error === 42; } called && boxed.valueOf() === 7 && boxed instanceof Number && calls === 2 && callThrow && constructThrow;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(69, "Boolean(0) === false && Boolean('x') === true;").as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            158,
            "Number.isNaN(NaN) && !Number.isNaN('NaN') && Number.isFinite(1.5) && !Number.isFinite(Infinity) && Number.isInteger(-0) && !Number.isInteger(1.5) && Number.isSafeInteger(9007199254740991) && !Number.isSafeInteger(9007199254740992);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            159,
            "let descriptor = Object.getOwnPropertyDescriptor(Number, 'MAX_SAFE_INTEGER'); Number.MAX_SAFE_INTEGER === 9007199254740991 && Number.MIN_SAFE_INTEGER === -9007199254740991 && Number.isNaN(Number.NaN) && Number.POSITIVE_INFINITY === Infinity && Number.NEGATIVE_INFINITY === -Infinity && Number.MAX_VALUE > 1e300 && Number.MIN_VALUE > 0 && descriptor.writable === false && descriptor.enumerable === false && descriptor.configurable === false;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            160,
            "let boxed = new Number(-3); let method = Number.prototype.valueOf; let descriptor = Object.getOwnPropertyDescriptor(Number.prototype, 'valueOf'); let rejected = false; let invalidRadix = false; try { method.call({}); } catch (error) { rejected = error instanceof TypeError; } try { boxed.toString(1); } catch (error) { invalidRadix = error instanceof RangeError; } boxed.valueOf() === -3 && boxed.toString() === '-3' && Number.prototype.valueOf() === 0 && (4).toString() === '4' && (255).toString(16) === 'ff' && (0.5).toString(2) === '0.1' && method.call(4) === 4 && boxed instanceof Number && Object.getPrototypeOf(boxed) === Number.prototype && Object.prototype.toString.call(boxed) === '[object Number]' && descriptor.value === method && descriptor.writable && !descriptor.enumerable && descriptor.configurable && rejected && invalidRadix;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            161,
            "Number.prototype.toString(2) === '0' && (new Number()).toString(2) === '0' && (new Number(0)).toString(2) === '0' && (new Number(-1)).toString(2) === '-1' && (new Number(1)).toString(2) === '1' && (new Number(Number.NaN)).toString(2) === 'NaN' && (new Number(Number.POSITIVE_INFINITY)).toString(2) === 'Infinity' && (new Number(Number.NEGATIVE_INFINITY)).toString(2) === '-Infinity';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            165,
            "let rangeError = false; let primitiveError = false; try { (3).toFixed(101); } catch (error) { rangeError = error instanceof RangeError; } try { Number.prototype.toFixed.call(0, { valueOf: undefined, toString: undefined }); } catch (error) { primitiveError = error instanceof TypeError; } (1.25).toFixed(1) === '1.3' && (1000000000000000128).toFixed(0) === '1000000000000000128' && Number.NaN.toFixed(2) === 'NaN' && (3).toFixed(2) === '3.00' && Number.prototype.toFixed.length === 1 && rangeError && primitiveError;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            166,
            "let rangeError = false; try { (3).toExponential(101); } catch (error) { rangeError = error instanceof RangeError; } (123.456).toExponential(3) === '1.235e+2' && (0.9999).toExponential(0) === '1e+0' && (25).toExponential(0) === '3e+1' && (0).toExponential(2) === '0.00e+0' && (100).toExponential() === '1e+2' && Infinity.toExponential(1000) === 'Infinity' && Number.prototype.toExponential.length === 1 && rangeError;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            167,
            "let rejected = false; let constructed = false; let symbol = Symbol('description'); try { +symbol; } catch (error) { rejected = error instanceof TypeError; } try { new Symbol(); } catch (error) { constructed = error instanceof TypeError; } typeof symbol === 'symbol' && symbol !== Symbol('description') && rejected && constructed;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            168,
            "let rangeError = false; try { (3).toPrecision(0); } catch (error) { rangeError = error instanceof RangeError; } (7).toPrecision(3) === '7.00' && (99.95).toPrecision(3) === '100' && (0.000001).toPrecision(3) === '0.00000100' && (0.0000001).toPrecision(2) === '1.0e-7' && (1.2345e27).toPrecision(6) === '1.23450e+27' && (42).toPrecision() === '42' && Infinity.toPrecision(1000) === 'Infinity' && rangeError;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            169,
            "let order = 0; let fallback = { valueOf() { order = order * 10 + 1; return {}; }, toString() { order = order * 10 + 2; return '2'; } }; let direct = { valueOf() { order = order * 10 + 3; return 1; }, toString() { order = 99; return 2; } }; let recovered = { valueOf() { try { throw 7; } catch (error) { return error - 6; } } }; let thrown = false; try { (1).toFixed({ valueOf() { throw 42; } }); } catch (error) { thrown = error === 42; } (1.25).toFixed(fallback) === '1.25' && (1.25).toFixed(direct) === '1.3' && (1.25).toFixed(recovered) === '1.3' && order === 123 && thrown;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies Object.prototype.toString reports the primitive and object tags used by harnesses.
#[test]
fn object_to_string_reports_standard_tags() {
    assert_eq!(
        execute_source(
            70,
            "Object.prototype.toString.call([]) === '[object Array]' && Object.prototype.toString.call(1) === '[object Number]' && Object.prototype.toString.call(null) === '[object Null]';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies Object.assign copies own data slots in source order and returns its target.
#[test]
fn object_assign_copies_own_data_properties() {
    assert_eq!(
        execute_source(
            71,
            "let target = {}; let source = { first: 1, second: 2 }; Object.assign(target, source); target.first + target.second;",
        )
        .as_i32(),
        Some(3),
    );
    assert_eq!(
        execute_source(72, "let source = { answer: 42 }; ({ ...source }).answer;").as_i32(),
        Some(42),
    );
    assert_eq!(
        execute_source(
            73,
            "let source = { first: 1, second: 2 }; Object.keys(source)[1] === 'second' && Object.values(source)[0] === 1 && Object.entries(source)[1][1] === 2 && Object.values('ab')[1] === 'b' && Object.entries('ab')[0][0] === '0';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            74,
            "let threw = false; try { Object.values(null); } catch (error) { threw = error instanceof TypeError; } threw;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            75,
            "let object = { answer: 42 }; Object.hasOwn(object, 'answer') && Object.is(NaN, NaN) && !Object.is(0, -0);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            76,
            "let prototype = { answer: 42 }; let object = Object.create(prototype, { own: { value: 7 } }); Object.getPrototypeOf(object) === prototype && prototype.isPrototypeOf(object) && object.answer === 42 && object.own === 7;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            77,
            "Object.create.length === 2 && Object.create.name === 'create' && Object.hasOwn(Object.create, 'length');",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Covers numeric key sorting, canonical uint32 boundaries, and function-key creation history.
#[test]
fn object_enumeration_uses_ordinary_own_property_key_order() {
    assert_eq!(
        execute_source(
            171,
            "let object = {}; object[9] = 9; object[2] = 2; object[10] = 10; object['01'] = 1; object[4294967294] = 4; object[4294967295] = 5; object.alpha = 6; Object.keys(object).join(',') === '2,9,10,4294967294,01,4294967295,alpha' && Object.getOwnPropertyNames(object).join(',') === '2,9,10,4294967294,01,4294967295,alpha';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            172,
            "let fn = () => {}; fn.a = 1; Object.defineProperty(fn, 'length', { enumerable: true }); let original = Object.keys(fn).join(',') === 'length,a'; delete fn.length; fn.length = 2; original && Object.keys(fn).join(',') === 'a,length';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Verifies the shared Function identity and the current always-extensible object policy.
#[test]
fn function_identity_and_object_extensibility_are_published() {
    assert_eq!(
        execute_source(
            78,
            "Function.prototype.constructor === Function && Object.getPrototypeOf(Function) === Function.prototype;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            79,
            "Object.isExtensible({}) && Object.isExtensible(function () {}) && !Object.isExtensible(1) && !Object.isExtensible(null);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

/// Covers preventExtensions identity, object state, and strict versus sloppy writes.
#[test]
fn prevent_extensions_blocks_new_own_properties() {
    assert_eq!(
        execute_source(
            80,
            "let object = { value: 1 }; Object.preventExtensions(object); object.added = 2; object.value = 3; !Object.isExtensible(object) && !Object.hasOwn(object, 'added') && object.value === 3 && Object.preventExtensions.length === 1 && Object.preventExtensions.name === 'preventExtensions';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            81,
            "function write(object) { 'use strict'; object.added = 2; } let object = {}; Object.preventExtensions(object); let threw = false; try { write(object); } catch (error) { threw = error instanceof TypeError; } threw && Object.preventExtensions(1) === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
    assert_eq!(
        execute_source(
            82,
            "let callable = function () {}; Object.preventExtensions(callable); let threw = false; try { Object.defineProperty(callable, 'added', { value: 1 }); } catch (error) { threw = error instanceof TypeError; } threw && !Object.isExtensible(callable);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers Object.setPrototypeOf identity, primitive behavior, and cycle/non-extensible failures.
fn object_set_prototype_of_uses_the_ordinary_mutation_contract() {
    assert_eq!(
        execute_source(
            1_032,
            "let prototype = { marker: 7 }; let object = {}; let identity = Object.setPrototypeOf(object, prototype) === object; let cycle = false; try { Object.setPrototypeOf(prototype, object); } catch (error) { cycle = error instanceof TypeError; } let frozen = {}; Object.preventExtensions(frozen); let blocked = false; try { Object.setPrototypeOf(frozen, prototype); } catch (error) { blocked = error instanceof TypeError; } identity && object.marker === 7 && cycle && blocked && Object.setPrototypeOf(1, null) === 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Forwards absent Proxy traps iteratively while preserving the outer Object builtin result.
fn nested_proxy_prototype_and_extensibility_methods_forward() {
    assert_eq!(
        execute_source(
            1_031,
            "let prototype = {}; let inner = new Proxy({}, { getPrototypeOf() { return prototype; }, isExtensible(target) { return Reflect.isExtensible(target); }, preventExtensions(target) { Object.preventExtensions(target); return true; } }); let outer = new Proxy(inner, { get getPrototypeOf() { return undefined; } }); let prototypeOk = Object.getPrototypeOf(outer) === prototype; let extensibleOk = Reflect.isExtensible(outer); let returned = Object.preventExtensions(outer); prototypeOk && extensibleOk && returned === outer && !Reflect.isExtensible(outer);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True),
    );
}

#[test]
/// Covers omitted arguments, explicit undefined, supplied values, and left-to-right defaults.
fn default_parameters_use_undefined_only_and_see_prior_parameters() {
    assert_eq!(
        execute_source(
            63,
            "function add(value = 40, next = value + 1) { return next; } add();",
        )
        .as_i32(),
        Some(41)
    );
    assert_eq!(
        execute_source(
            64,
            "function add(value = 40, next = value + 1) { return next; } add(undefined);",
        )
        .as_i32(),
        Some(41)
    );
    assert_eq!(
        execute_source(
            65,
            "function add(value = 40, next = value + 1) { return next; } add(null);",
        )
        .as_i32(),
        Some(1),
    );
    assert_eq!(
        execute_source(
            66,
            "function add(value = 40, next = value + 1) { return next; } add(10);",
        )
        .as_i32(),
        Some(11)
    );
}

#[test]
/// Checks update results and one-shot object/key evaluation through source compilation.
fn computed_members_preserve_reference_evaluation_and_updates() {
    assert_eq!(
        execute_source(
            40,
            "function Box() { this[0] = 40; } let box = new Box(); box[0]++; box[0] += 1; box[0];",
        )
        .as_i32(),
        Some(42)
    );
    assert_eq!(
        execute_source(
            41,
            "function Box() { this[0] = 1; this.calls = 0; } function target(receiver) { receiver.calls += 1; return receiver; } function key(receiver) { receiver.calls += 1; return 0; } let box = new Box(); target(box)[key(box)] += 2; box.calls === 2 && box[0] === 3;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Exercises observable string primitives instead of inspecting internal GC descriptors.
fn typeof_and_string_constants_follow_primitive_semantics() {
    assert_eq!(
        execute_source(
            42,
            "typeof undefined === 'undefined' && typeof null === 'object' && typeof true === 'boolean' && typeof 1 === 'number' && typeof 'x' === 'string';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            43,
            "function Box() {} typeof Box === 'function' && typeof new Box() === 'object' && 'same' === 'same' && !'';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            214,
            "typeof [] === 'object' && typeof new Number(1) === 'object' && typeof function() {} === 'function';",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Covers the Math family registration, corner-case signed zero, and numeric globals.
fn math_family_and_number_globals_are_available() {
    assert_eq!(
        execute_source(
            901,
            "Math.abs(-3) === 3 && Math.floor(1.9) === 1 && Math.ceil(1.1) === 2 && Math.trunc(-1.9) === -1 && Math.round(-0.5) === 0 && 1 / Math.round(-0.5) === -Infinity && Math.min(0, -0) === -0 && Math.max(-0, 0) === 0 && Math.hypot(3, 4) === 5 && Math.imul(0xffffffff, 5) === -5 && Math.clz32(1) === 31 && Math.random() >= 0 && Math.random() < 1;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            902,
            "isNaN(NaN) && !isNaN(1) && isFinite(1) && !isFinite(Infinity) && parseInt('  -0x10tail') === -16 && parseInt('11', 2) === 3 && parseFloat(' -1.25e2tail') === -125;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            903,
            "let finite = { valueOf() { return '2'; } }; let nan = { valueOf() { return 'x'; } }; isFinite(finite) && isNaN(nan);",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
    assert_eq!(
        execute_source(
            904,
            "new EvalError() instanceof EvalError && new URIError() instanceof URIError;",
        )
        .as_immediate(),
        Some(tachyon_value::Immediate::True)
    );
}

#[test]
/// Verifies Array iterator creation and both completion paths remain resumable.
fn array_iterator_next_completes_array_like_source() {
    assert_eq!(
        execute_source(
            909,
            "let stage = 0; try { let source = { length: 1, 0: 7 }; stage = 1; let iterator = Array.prototype.values.call(source); stage = 2; let first = iterator.next(); stage = 3; first.value; stage = 4; let second = iterator.next(); stage = 5; second.done; stage = 6; } catch (error) {} stage;",
        )
        .as_i32(),
        Some(6)
    );
}
