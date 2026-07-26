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
