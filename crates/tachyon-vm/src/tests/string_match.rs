use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const STRING_MATCH_PROTOCOL_SOURCE: &str = r#"
var failure = 0;
var order = "";
var rawReceiver = { toString() { order += "s"; return "unused"; } };
var customMatch = {};
Object.defineProperty(customMatch, Symbol.match, {
  get() {
    order += "g";
    return function(value) {
      order += this === customMatch && value === rawReceiver ? "c" : "x";
      return 41;
    };
  }
});
if (String.prototype.match.call(rawReceiver, customMatch) !== 41 || order !== "gc") failure = 1;

var marker = {};
var getterThrow = false;
try { "x".match(new Proxy({}, { get() { throw marker; } })); }
catch (error) { getterThrow = error === marker; }
if (!getterThrow) failure = 2;

var callThrow = false;
try { "x".match({ [Symbol.match]() { throw marker; } }); }
catch (error) { callThrow = error === marker; }
if (!callThrow) failure = 3;

order = "";
var fallbackPattern = {
  get [Symbol.match]() { order += "g"; return null; },
  toString() { order += "p"; return "b"; }
};
var fallbackReceiver = { toString() { order += "s"; return "abc"; } };
var fallbackMatch = String.prototype.match.call(fallbackReceiver, fallbackPattern);
if (fallbackMatch[0] !== "b" || fallbackMatch.index !== 1 || order !== "gsp") failure = 4;

var nonCallable = false;
order = "";
try {
  String.prototype.match.call({ toString() { order += "s"; return "x"; } }, {
    get [Symbol.match]() { order += "g"; return 1; }
  });
} catch (error) { nonCallable = error instanceof TypeError; }
if (!nonCallable || order !== "g") failure = 5;

order = "";
var matchAllPattern = {};
Object.defineProperty(matchAllPattern, Symbol.match, {
  get() { order += "i"; return true; }
});
Object.defineProperty(matchAllPattern, "flags", {
  get() { order += "f"; return { toString() { order += "t"; return "g"; } }; }
});
Object.defineProperty(matchAllPattern, Symbol.matchAll, {
  get() {
    order += "m";
    return function(value) {
      order += this === matchAllPattern && value === rawReceiver ? "c" : "x";
      return 73;
    };
  }
});
if (String.prototype.matchAll.call(rawReceiver, matchAllPattern) !== 73 || order !== "iftmc") failure = 6;

order = "";
var falsePattern = {
  get [Symbol.match]() { order += "i"; return false; },
  get flags() { order += "bad"; throw marker; },
  get [Symbol.matchAll]() { order += "m"; return function(value) { order += "c"; return value; }; }
};
if ("ok".matchAll(falsePattern) !== "ok" || order !== "imc") failure = 7;

order = "";
var globalThrow = false;
try {
  "x".matchAll({
    get [Symbol.match]() { order += "i"; return true; },
    get flags() { order += "f"; return null; },
    get [Symbol.matchAll]() { order += "bad"; return function() {}; }
  });
} catch (error) { globalThrow = error instanceof TypeError; }
if (!globalThrow || order !== "if") failure = 8;

order = "";
var fallbackAllPattern = {
  get [Symbol.match]() { order += "i"; return false; },
  get [Symbol.matchAll]() { order += "m"; return null; },
  toString() { order += "p"; return "a"; }
};
var fallbackAllReceiver = { toString() { order += "s"; return "aba"; } };
var fallbackAll = String.prototype.matchAll.call(fallbackAllReceiver, fallbackAllPattern);
var first = fallbackAll.next();
var second = fallbackAll.next();
if (first.value[0] !== "a" || first.value.index !== 0 || second.value.index !== 2 || order !== "imsp") failure = 9;

var originalMatch = RegExp.prototype[Symbol.match];
order = "";
Object.defineProperty(RegExp.prototype, Symbol.match, {
  configurable: true,
  get() { order += "v"; return function(value) { order += "c"; return value; }; }
});
if ("abc".match("b") !== "abc" || order !== "vc") failure = 10;
Object.defineProperty(RegExp.prototype, Symbol.match, {
  configurable: true, writable: true, value: originalMatch
});

failure;
"#;

const STRING_MATCH_GC_SOURCE: &str = r#"
var originalMatch = RegExp.prototype[Symbol.match];
var originalMatchAll = RegExp.prototype[Symbol.matchAll];
var order = "";
Object.defineProperty(RegExp.prototype, Symbol.match, {
  configurable: true,
  get() { order += "g"; return function(value) { order += "c"; return value; }; }
});
Object.defineProperty(RegExp.prototype, Symbol.matchAll, {
  configurable: true,
  get() { order += "a"; return function(value) { order += "d"; return value; }; }
});
var receiver = { toString() { order += "s"; return "abc"; } };
var match = String.prototype.match.call(receiver, "b");
var matchAll = String.prototype.matchAll.call(receiver, "b");
var pattern = {};
Object.defineProperty(pattern, Symbol.match, { get() { order += "i"; return true; } });
Object.defineProperty(pattern, "flags", {
  get() { order += "f"; return { toString() { order += "t"; return "g"; } }; }
});
Object.defineProperty(pattern, Symbol.matchAll, {
  get() { order += "m"; return function(value) { order += "x"; return value; }; }
});
var custom = String.prototype.matchAll.call(receiver, pattern);
Object.defineProperty(RegExp.prototype, Symbol.match, {
  configurable: true, writable: true, value: originalMatch
});
Object.defineProperty(RegExp.prototype, Symbol.matchAll, {
  configurable: true, writable: true, value: originalMatchAll
});
match === "abc" && matchAll === "abc" && custom === receiver && order === "sgcsadiftmx";
"#;

#[test]
fn string_match_protocol_covers_dispatch_and_gc_matrix() {
    assert_string_match_protocol::<1>(false);
    assert_string_match_protocol::<2>(false);
    assert_string_match_protocol::<4>(false);
    assert_string_match_protocol::<8>(false);
    assert_string_match_protocol::<16>(false);
    assert_string_match_gc::<8>();
}

/// Exercises every new traced edge while forcing relocation at each allocation.
fn assert_string_match_gc<const N: usize>() {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_500 + N as u32),
                SourceName::new("string-match-gc-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_MATCH_GC_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String match GC fixture compiles");
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .expect("String match GC fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major dispatch batch {N} returned {outcome:?}"
    );
}

/// Runs the complete observable protocol fixture under one dispatch/GC policy.
fn assert_string_match_protocol<const N: usize>(force_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(10_200 + N as u32 + u32::from(force_major) * 32),
                SourceName::new("string-match-protocol-fixture"),
                MediaType::JavaScript,
                Arc::from(STRING_MATCH_PROTOCOL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("String match protocol fixture compiles");
    let mut isolate = test_isolate();
    if force_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 524_288,
                quantum: 524_288,
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "String match protocol fixture executes for N={N}, forced_major={force_major}: {error:?}"
            )
        });
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, forced_major={force_major} returned {outcome:?}"
    );
}
