use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const REGEXP_EXEC_SOURCE: &str = r#"
var order = "";
var receiver = {};
var customThis = false;
var customArgument = false;
Object.defineProperty(receiver, "exec", {
  get: function() {
    order += "g";
    return function(value) {
      order += "c";
      customThis = this === receiver;
      customArgument = value === "needle";
      return {};
    };
  }
});
var input = {
  toString: function() {
    order += "s";
    return "needle";
  }
};
var customMatched = RegExp.prototype.test.call(receiver, input);

var nullReceiver = { exec: function(value) { return value === "x" ? null : {}; } };
var customMissed = RegExp.prototype.test.call(nullReceiver, "x");

var invalidResultTypeError = false;
try {
  RegExp.prototype.test.call({ exec: function() { return 1; } }, "x");
} catch (error) {
  invalidResultTypeError = error instanceof TypeError;
}

var getterError = {};
var getterAbrupt = false;
var getterReceiver = {};
Object.defineProperty(getterReceiver, "exec", {
  get: function() { throw getterError; }
});
try {
  RegExp.prototype.test.call(getterReceiver, "x");
} catch (error) {
  getterAbrupt = error === getterError;
}

var callError = {};
var callAbrupt = false;
try {
  RegExp.prototype.test.call({ exec: function() { throw callError; } }, "x");
} catch (error) {
  callAbrupt = error === callError;
}

var skippedGetter = true;
var inputError = {};
var conversionAbrupt = false;
var orderingReceiver = {};
Object.defineProperty(orderingReceiver, "exec", {
  get: function() { skippedGetter = false; return function() { return null; }; }
});
try {
  RegExp.prototype.test.call(orderingReceiver, {
    toString: function() { throw inputError; }
  });
} catch (error) {
  conversionAbrupt = error === inputError;
}

var genuine = /needle/;
genuine.exec = 1;
var genuineFallback = genuine.test("needle");
var incompatibleTypeError = false;
try {
  RegExp.prototype.test.call({ exec: 1 }, "needle");
} catch (error) {
  incompatibleTypeError = error instanceof TypeError;
}

var proxyOrder = "";
var proxy;
var proxyTarget = {
  exec: function(value) {
    proxyOrder += this === proxy && value === "p" ? "c" : "bad";
    return null;
  }
};
proxy = new Proxy(proxyTarget, {
  get: function(target, key, receiverValue) {
    proxyOrder += key === "exec" && receiverValue === proxy ? "g" : "bad";
    return target[key];
  }
});
var proxyMissed = RegExp.prototype.test.call(proxy, "p");

var callableProxyThis = false;
var callableProxyArgument = false;
var callableExec = new Proxy(function(value) {
  callableProxyThis = this === callableProxyReceiver;
  callableProxyArgument = value === "callable";
  return {};
}, {});
var callableProxyReceiver = { exec: callableExec };
var callableProxyMatched = RegExp.prototype.test.call(callableProxyReceiver, "callable");

var primitiveReceiverBeforeInput = false;
var primitiveInputObserved = false;
try {
  RegExp.prototype.test.call(null, {
    toString: function() { primitiveInputObserved = true; return "x"; }
  });
} catch (error) {
  primitiveReceiverBeforeInput = error instanceof TypeError;
}

customMatched && !customMissed && order === "sgc" && customThis && customArgument &&
invalidResultTypeError && getterAbrupt && callAbrupt && conversionAbrupt && skippedGetter &&
genuineFallback && incompatibleTypeError && !proxyMissed && proxyOrder === "gc" &&
callableProxyMatched && callableProxyThis && callableProxyArgument &&
primitiveReceiverBeforeInput && !primitiveInputObserved;
"#;

const REGEXP_LAST_INDEX_SOURCE: &str = r#"
var nonGlobalReads = 0;
var nonGlobal = /a/;
var retainedIndex = {
  valueOf: function() { nonGlobalReads++; return 99; }
};
nonGlobal.lastIndex = retainedIndex;
var nonGlobalResult = nonGlobal.exec("a");

var globalReads = 0;
var global = /a/g;
global.lastIndex = {
  valueOf: function() { globalReads++; return 0; }
};
var globalResult = global.exec("a");

var failed = /a/g;
failed.lastIndex = { valueOf: function() { return 42; } };
var failedResult = failed.exec("x");

var abruptMarker = {};
var abrupt = /a/;
abrupt.lastIndex = { valueOf: function() { throw abruptMarker; } };
var abruptPreserved = false;
try { abrupt.exec("a"); } catch (error) { abruptPreserved = error === abruptMarker; }

var readOnly = /a/g;
Object.defineProperty(readOnly, "lastIndex", { writable: false });
var strictSet = false;
try { readOnly.exec("a"); } catch (error) { strictSet = error instanceof TypeError; }

var testOnly = /a/g;
testOnly.exec = 1;
testOnly.lastIndex = { valueOf: function() { return 0; } };
var testMatched = testOnly.test("a");

nonGlobalResult[0] === "a" && nonGlobalReads === 1 &&
nonGlobal.lastIndex === retainedIndex && globalResult[0] === "a" &&
global.lastIndex === 1 && globalReads === 1 && failedResult === null &&
failed.lastIndex === 0 && abruptPreserved && strictSet && testMatched &&
testOnly.lastIndex === 1;
"#;

const REGEXP_INDICES_UNICODE_SOURCE: &str = r#"
var result = /(?:(?<left>a)|(b))(?<tail>c)?/d.exec("ac");
var plain = /(z)?/d.exec("");
var duplicate = /(?:(?<x>a)|(?<x>b))/d.exec("b");
var sameSpan = /((?<same>a))/d.exec("a");
var unicode = /./dug;
var unicodeResult = unicode.exec("\ud834\udf06");
var hanU = /\p{Script=Han}/du.exec("\ud842\udfb7a");
var hanV = /\p{Script=Han}/dv.exec("\ud842\udfb7a");

result[0] === "ac" && result[1] === "a" && result[2] === undefined &&
result[3] === "c" && result.index === 0 && result.input === "ac" &&
Object.getPrototypeOf(result.groups) === null && result.groups.left === "a" &&
result.groups.tail === "c" && result.indices.length === 4 &&
result.indices[0][0] === 0 && result.indices[0][1] === 2 &&
result.indices[1][0] === 0 && result.indices[1][1] === 1 &&
result.indices[2] === undefined && result.indices[3][0] === 1 &&
result.indices[3][1] === 2 && Object.getPrototypeOf(result.indices.groups) === null &&
result.indices.groups.left === result.indices[1] &&
result.indices.groups.tail === result.indices[3] && plain.groups === undefined &&
plain.indices.groups === undefined && duplicate.groups.x === "b" &&
duplicate.indices.groups.x === duplicate.indices[2] && unicodeResult[0].length === 2 &&
sameSpan.indices.groups.same === sameSpan.indices[2] &&
sameSpan.indices.groups.same !== sameSpan.indices[1] &&
unicodeResult.indices[0][0] === 0 && unicodeResult.indices[0][1] === 2 &&
unicode.lastIndex === 2 && hanU[0].length === 2 && hanU.indices[0][1] === 2 &&
hanV[0].length === 2 && hanV.indices[0][1] === 2;
"#;

#[test]
fn regexp_exec_protocol_works_for_every_dispatch_batch() {
    assert_regexp_exec::<1>(false);
    assert_regexp_exec::<2>(false);
    assert_regexp_exec::<4>(false);
    assert_regexp_exec::<8>(false);
    assert_regexp_exec::<16>(false);
}

#[test]
fn regexp_exec_state_survives_forced_major_collection() {
    for (index, (name, source)) in [
        (
            "custom-call",
            "RegExp.prototype.test.call({ exec: function(s) { return s === 'x' ? {} : null; } }, 'x');",
        ),
        (
            "input-conversion",
            "var order=''; var r={exec:function(s){order+='c';return {};}}; var ok=RegExp.prototype.test.call(r,{toString:function(){order+='s';return 'x';}}); ok && order==='sc';",
        ),
        (
            "exec-getter",
            "var order=''; var r={}; Object.defineProperty(r,'exec',{get:function(){order+='g';return function(){order+='c';return null;};}}); !RegExp.prototype.test.call(r,'x') && order==='gc';",
        ),
        (
            "builtin-fallback",
            "var r=/x/; r.exec=1; r.test('x');",
        ),
        (
            "proxy-get",
            "var p; var t={exec:function(){return this===p?null:{};}}; p=new Proxy(t,{get:function(t,k,r){return t[k];}}); !RegExp.prototype.test.call(p,'x');",
        ),
        (
            "exec-object-input",
            "var order=''; var result=/(a)(?<tail>b)/.exec({toString:function(){order+='s';return 'zab';}}); order==='s' && result[0]==='ab' && result[1]==='a' && result.groups.tail==='b' && result.index===1;",
        ),
        (
            "exec-valueof-fallback",
            "var result=/x/.exec({toString:function(){return {};},valueOf:function(){return 'x';}}); result!==null && result[0]==='x';",
        ),
        (
            "exec-function-input",
            "function sample(){}; /x/.exec(sample)===null;",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_forced_regexp_exec_slice(name, source, index as u32);
    }
}

#[test]
fn regexp_last_index_protocol_works_for_every_dispatch_batch_and_major_gc() {
    assert_regexp_last_index::<1>(false);
    assert_regexp_last_index::<2>(false);
    assert_regexp_last_index::<4>(false);
    assert_regexp_last_index::<8>(false);
    assert_regexp_last_index::<16>(false);
    assert_regexp_last_index::<1>(true);
    assert_regexp_last_index::<2>(true);
    assert_regexp_last_index::<4>(true);
    assert_regexp_last_index::<8>(true);
    assert_regexp_last_index::<16>(true);
}

#[test]
fn regexp_indices_and_unicode_work_for_every_dispatch_batch_and_major_gc() {
    assert_regexp_indices_unicode::<1>(false);
    assert_regexp_indices_unicode::<2>(false);
    assert_regexp_indices_unicode::<4>(false);
    assert_regexp_indices_unicode::<8>(false);
    assert_regexp_indices_unicode::<16>(false);
    assert_regexp_indices_unicode::<1>(true);
    assert_regexp_indices_unicode::<2>(true);
    assert_regexp_indices_unicode::<4>(true);
    assert_regexp_indices_unicode::<8>(true);
    assert_regexp_indices_unicode::<16>(true);
}

/// Runs `d` result publication and full-Unicode matching under one VM policy.
fn assert_regexp_indices_unicode<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(8_000 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("regexp-indices-unicode-fixture"),
                MediaType::JavaScript,
                Arc::from(REGEXP_INDICES_UNICODE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RegExp indices/Unicode fixture compiles");
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("RegExp indices/Unicode fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Runs observable lastIndex conversion and strict writes under one dispatch/GC policy.
fn assert_regexp_last_index<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_950 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("regexp-last-index-fixture"),
                MediaType::JavaScript,
                Arc::from(REGEXP_LAST_INDEX_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RegExp lastIndex fixture compiles");
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("RegExp lastIndex fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Executes one isolated protocol stage under forced major collection for precise rooting failures.
fn assert_forced_regexp_exec_slice(name: &str, source: &'static str, index: u32) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_900 + index),
                SourceName::new("regexp-exec-forced-major"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("{name} fixture compiles: {error:?}"));
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .unwrap_or_else(|error| panic!("{name} fixture executes: {error:?}"));
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "{name} forced-major returned {outcome:?}, kind={thrown_kind:?}"
    );
}

/// Compiles and executes the custom-exec protocol fixture under one dispatch/collection policy.
fn assert_regexp_exec<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(7_800 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("regexp-exec-fixture"),
                MediaType::JavaScript,
                Arc::from(REGEXP_EXEC_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("RegExp exec fixture compiles");
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 262_144,
                quantum: 262_144,
            },
        )
        .expect("RegExp exec fixture executes");
    let thrown_kind = match outcome {
        RunOutcome::Thrown(value) => isolate.native_error_kind(value).unwrap(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, kind={thrown_kind:?}"
    );
}
