use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};
use tachyon_gc::{ForcedCollectionMode, HeapLimit, SPAN_SIZE_BYTES};

use super::*;

const PRETTY_JSON_SOURCE: &str = r#"
var nested = JSON.stringify({a: [1, {b: 2}]}, null, "xy");
var expected = "{\nxy\"a\": [\nxyxy1,\nxyxy{\nxyxyxy\"b\": 2\nxyxy}\nxy]\n}";
nested === expected &&
JSON.stringify({a: 1}, null, "0123456789ignored") ===
  "{\n0123456789\"a\": 1\n}" &&
JSON.stringify({a: 1}, null, 4.99) === "{\n    \"a\": 1\n}" &&
JSON.stringify({a: 1}, null, 10) === JSON.stringify({a: 1}, null, 100) &&
JSON.stringify({a: 1}, null, Infinity) === JSON.stringify({a: 1}, null, 10) &&
JSON.stringify({a: 1}, null, NaN) === '{"a":1}' &&
JSON.stringify({a: 1}, null, -Infinity) === '{"a":1}' &&
JSON.stringify({a: 1}, null, -3.75) === '{"a":1}' &&
(function () {
  var numberSpace = new Number(1);
  numberSpace.toString = function () { throw "number-toString"; };
  numberSpace.valueOf = function () { return 3.9; };
  if (JSON.stringify({a: 1}, null, numberSpace) !==
      JSON.stringify({a: 1}, null, 3)) return false;
  var stringSpace = new String("unused");
  stringSpace.toString = function () { return "zz"; };
  stringSpace.valueOf = function () { throw "string-valueOf"; };
  if (JSON.stringify({a: 1}, null, stringSpace) !==
      JSON.stringify({a: 1}, null, "zz")) return false;
  var marker = {};
  var abrupt = new Number(4);
  abrupt.valueOf = function () { throw marker; };
  try { JSON.stringify({root: {value: 1}}, null, abrupt); }
  catch (error) { return error === marker; }
  return false;
})();
"#;

const RESUMABLE_JSON_SOURCE: &str = r#"
(function () {
  var order = [];
  var marker = {};
  var target = {
    a: {
      toJSON: function (key) {
        order.push("toJSON:" + key);
        return 3;
      }
    },
    b: undefined,
    c: 5
  };
  var proxy = new Proxy(target, {
    ownKeys: function () { order.push("ownKeys"); return ["a", "b", "c"]; },
    getOwnPropertyDescriptor: function (object, key) {
      order.push("descriptor:" + key);
      return Object.getOwnPropertyDescriptor(object, key);
    },
    get: function (object, key) { order.push("get:" + key); return object[key]; }
  });
  var text = JSON.stringify(proxy, function (key, value) {
    order.push("replacer:" + key);
    return key === "c" ? new Number(value + 1) : value;
  });
  if (text !== '{"a":3,"c":6}') return false;
  if (order.join("|") !==
      "get:toJSON|replacer:|ownKeys|descriptor:a|get:a|toJSON:a|replacer:a|" +
      "descriptor:b|get:b|replacer:b|descriptor:c|get:c|replacer:c") return false;

  var listLog = [];
  var list = new Proxy([new String("a"), new Number(2), "a", undefined], {
    get: function (object, key) { listLog.push(String(key)); return object[key]; }
  });
  if (JSON.stringify({a: 1, 2: 2, b: 3}, list) !== '{"a":1,"2":2}') return false;
  if (listLog.join("|") !== "length|0|1|2|3") return false;
  if (JSON.stringify([undefined, function () {}, Symbol("x")]) !== "[null,null,null]") {
    return false;
  }
  var abrupt = { toJSON: function () { throw marker; } };
  try { JSON.stringify({abrupt: abrupt}); }
  catch (error) { return error === marker; }
  return false;
})()
"#;

const FORCED_JSON_SOURCE: &str = r#"
(function () {
  var order = [];
  var target = {
    a: {toJSON: function (key) { order.push("toJSON:" + key); return 4; }},
    b: 2,
    c: "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
  };
  var proxy = new Proxy(target, {
    ownKeys: function () { order.push("ownKeys"); return ["a", "b", "c"]; },
    getOwnPropertyDescriptor: function (object, key) {
      order.push("descriptor:" + key);
      return Object.getOwnPropertyDescriptor(object, key);
    },
    get: function (object, key) { order.push("get:" + key); return object[key]; }
  });
  var text = JSON.stringify(proxy, function (key, value) {
    order.push("replacer:" + key);
    return value;
  });
  return text.indexOf('{"a":4,"b":2,"c":"xxxx') === 0 && text.length > 150 && order.length === 13;
})()
"#;

const FORCED_PROPERTY_LIST_SOURCE: &str = r#"
(function () {
  var log = [];
  var entries = [];
  for (var i = 0; i < 12; i++) entries[i] = i % 3 === 0 ? new String("a") : i;
  var list = new Proxy(entries, {
    get: function (object, key) { log.push(String(key)); return object[key]; }
  });
  var text = JSON.stringify({a: 1, 1: 2, 2: 3, 4: 4, 5: 5, 7: 7, 8: 8, 10: 10, 11: 11}, list);
  return text === '{"a":1,"1":2,"2":3,"4":4,"5":5,"7":7,"8":8,"10":10,"11":11}' &&
    log.length === 13 && log[0] === "length" && log[12] === "11";
})()
"#;

const LARGE_PROPERTY_LIST_SOURCE: &str = r#"
(function () {
  var replacer = [];
  for (var i = 0; i < 4096; i++) replacer.push(i);
  return JSON.stringify({"foopy": "FAIL", "4093": 17}, replacer) === '{"4093":17}';
})()
"#;

const DENSE_PRIMITIVE_JSON_SOURCE: &str = r#"
(function () {
  var object = {};
  var replacer = [];
  var objectExpected = "{";
  for (var i = 0; i < 4096; i++) {
    object[i] = i;
    replacer.push(i);
    if (i !== 0) objectExpected += ",";
    objectExpected += '"' + i + '":' + i;
  }
  objectExpected += "}";
  if (JSON.stringify(object, replacer) !== objectExpected) return false;

  var array = [];
  var arrayExpected = "[";
  for (var j = 0; j < 4096; j++) {
    var kind = j % 4;
    var value = kind === 0 ? null : kind === 1 ? true : kind === 2 ? "v" : j;
    array[j] = value;
    if (j !== 0) arrayExpected += ",";
    arrayExpected += kind === 0 ? "null" : kind === 1 ? "true" : kind === 2 ? '"v"' : String(j);
  }
  arrayExpected += "]";
  return JSON.stringify(array) === arrayExpected;
})()
"#;

const FORCED_DENSE_PRIMITIVE_JSON_SOURCE: &str = r#"
(function () {
  var object = {};
  var replacer = [];
  for (var i = 0; i < 4096; i++) {
    object[i] = i;
    replacer.push(i);
  }
  var objectText = JSON.stringify(object, replacer);
  if (objectText.indexOf('{"0":0,"1":1') !== 0 ||
      objectText.indexOf('"2048":2048') < 0 ||
      objectText.indexOf('"4095":4095}') !== objectText.length - 12) return false;

  var array = [];
  for (var j = 0; j < 4096; j++) {
    var kind = j % 4;
    array[j] = kind === 0 ? null : kind === 1 ? true : kind === 2 ? "v" : j;
  }
  var arrayText = JSON.stringify(array);
  return arrayText.indexOf('[null,true,"v",3') === 0 &&
    arrayText.indexOf('null,true,"v",4095]') === arrayText.length - 19;
})()
"#;

#[test]
fn primitive_string_and_number_indentation_is_stable_for_every_dispatch_batch() {
    assert_pretty_json_batch::<1>(false);
    assert_pretty_json_batch::<2>(false);
    assert_pretty_json_batch::<4>(false);
    assert_pretty_json_batch::<8>(false);
    assert_pretty_json_batch::<16>(false);
}

#[test]
fn primitive_string_and_number_indentation_survives_forced_major_collection() {
    assert_pretty_json_batch::<8>(true);
}

#[test]
fn resumable_stringify_is_stable_for_every_dispatch_batch() {
    assert_resumable_json_batch::<1>(None);
    assert_resumable_json_batch::<2>(None);
    assert_resumable_json_batch::<4>(None);
    assert_resumable_json_batch::<8>(None);
    assert_resumable_json_batch::<16>(None);
}

#[test]
fn resumable_stringify_survives_forced_collections_and_growth() {
    assert_resumable_json_batch::<8>(Some(ForcedCollectionMode::Minor));
    assert_resumable_json_batch::<8>(Some(ForcedCollectionMode::Major));
    assert_forced_property_list::<8>(ForcedCollectionMode::Minor);
    assert_forced_property_list::<8>(ForcedCollectionMode::Major);
}

#[test]
fn large_property_list_is_iterative_for_every_dispatch_batch() {
    assert_large_property_list::<1>(None);
    assert_large_property_list::<2>(None);
    assert_large_property_list::<4>(None);
    assert_large_property_list::<8>(None);
    assert_large_property_list::<16>(None);
}

#[test]
fn large_property_list_survives_forced_collections() {
    assert_large_property_list::<8>(Some(ForcedCollectionMode::Minor));
    assert_large_property_list::<8>(Some(ForcedCollectionMode::Major));
}

#[test]
fn dense_primitive_json_is_iterative_for_every_dispatch_batch() {
    assert_dense_primitive_json::<1>(None);
    assert_dense_primitive_json::<2>(None);
    assert_dense_primitive_json::<4>(None);
    assert_dense_primitive_json::<8>(None);
    assert_dense_primitive_json::<16>(None);
}

#[test]
fn dense_primitive_json_survives_forced_collections() {
    assert_dense_primitive_json::<8>(Some(ForcedCollectionMode::Minor));
    assert_dense_primitive_json::<8>(Some(ForcedCollectionMode::Major));
}

#[test]
fn hex_escape_digits_are_ascii_only() {
    assert_eq!(hex_value(u16::from(b'0')), Some(0));
    assert_eq!(hex_value(u16::from(b'f')), Some(15));
    assert_eq!(hex_value(u16::from(b'G')), None);
}

/// Runs nested primitive JSON indentation under one dispatch and collection policy.
fn assert_pretty_json_batch<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(1_900 + N as u32),
                SourceName::new("json-primitive-indentation"),
                MediaType::JavaScript,
                Arc::from(PRETTY_JSON_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("JSON indentation fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(9 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024),
    ))
    .expect("JSON indentation isolate initializes");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("JSON indentation fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "JSON indentation batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Runs callback-heavy JSON serialization under one dispatch and collection policy.
fn assert_resumable_json_batch<const N: usize>(forced: Option<ForcedCollectionMode>) {
    let source = if forced.is_some() {
        FORCED_JSON_SOURCE
    } else {
        RESUMABLE_JSON_SOURCE
    };
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_000 + N as u32),
                SourceName::new("json-resumable-stringify"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("resumable JSON fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(3, 4)),
        HeapLimit::new(256 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("resumable JSON isolate initializes");
    if let Some(mode) = forced {
        isolate.heap.set_forced_collection_mode(mode);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("resumable JSON fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "resumable JSON batch {N}, forced={forced:?} returned {outcome:?}"
    );
}

/// Runs Proxy/boxed/deduplicated property-list growth under forced collections.
fn assert_forced_property_list<const N: usize>(forced: ForcedCollectionMode) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_100 + N as u32),
                SourceName::new("json-forced-property-list"),
                MediaType::JavaScript,
                Arc::from(FORCED_PROPERTY_LIST_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("forced property-list fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(2_048, 2 * 1024 * 1024, AtomHashSeed::new(5, 6)),
        HeapLimit::new(256 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 2_048),
    ))
    .expect("forced property-list isolate initializes");
    isolate.heap.set_forced_collection_mode(forced);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("forced property-list fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced property-list batch {N}, forced={forced:?} returned {outcome:?}"
    );
}

/// Runs the Test262-sized replacer list without relying on the native call stack.
fn assert_large_property_list<const N: usize>(forced: Option<ForcedCollectionMode>) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_200 + N as u32),
                SourceName::new("json-large-property-list"),
                MediaType::JavaScript,
                Arc::from(LARGE_PROPERTY_LIST_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("large property-list fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(8_192, 8 * 1024 * 1024, AtomHashSeed::new(7, 8)),
        HeapLimit::new(8_192 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 8_192),
    ))
    .expect("large property-list isolate initializes");
    if let Some(mode) = forced {
        isolate.heap.set_forced_collection_mode(mode);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 1_000_000,
                quantum: 1_000_000,
            },
        )
        .expect("large property-list fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "large property-list batch {N}, forced={forced:?} returned {outcome:?}"
    );
}

/// Runs dense Object and Array primitive serialization without native recursion.
fn assert_dense_primitive_json<const N: usize>(forced: Option<ForcedCollectionMode>) {
    let source = if forced.is_some() {
        FORCED_DENSE_PRIMITIVE_JSON_SOURCE
    } else {
        DENSE_PRIMITIVE_JSON_SOURCE
    };
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_300 + N as u32),
                SourceName::new("json-dense-primitives"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("dense primitive JSON fixture compiles");
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(8_192, 8 * 1024 * 1024, AtomHashSeed::new(9, 10)),
        HeapLimit::new(16_384 * SPAN_SIZE_BYTES),
        StackLimits::new(96, 8_192),
        RealmLimits::new(96, 8_192),
    ))
    .expect("dense primitive JSON isolate initializes");
    if let Some(mode) = forced {
        isolate.heap.set_forced_collection_mode(mode);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_000_000,
                quantum: 4_000_000,
            },
        )
        .expect("dense primitive JSON fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dense primitive JSON batch {N}, forced={forced:?} returned {outcome:?}"
    );
}
