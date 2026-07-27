use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const BIGINT_PRIMITIVE_SOURCE: &str = r#"
typeof 0n === "bigint"
    && !0n
    && !!1n
    && 140737488355327n === 140737488355327n
    && 140737488355328n === 140737488355328n
    && 140737488355328n !== 140737488355329n
    && (140737488355328n + "") === "140737488355328"
    && (340282366920938463463374607431768211455n + "")
        === "340282366920938463463374607431768211455"
    && -140737488355328n === -140737488355328n
    && (-18446744073709551617n + "") === "-18446744073709551617";
"#;

const BIGINT_CONVERSION_SOURCE: &str = r#"
var marker = {};
var trace = "";
var once = {
  [Symbol.toPrimitive](hint) {
    trace += hint;
    return "18446744073709551616";
  }
};
var getter = {};
Object.defineProperty(getter, Symbol.toPrimitive, {
  get() {
    trace += "g";
    return function(hint) { trace += hint; return true; };
  }
});
var thrownIdentity = false;
try {
  BigInt({ [Symbol.toPrimitive]() { throw marker; } });
} catch (error) {
  thrownIdentity = error === marker;
}
var constructCoerced = false;
var constructRejected = false;
try {
  new BigInt({ valueOf() { constructCoerced = true; return 1; } });
} catch (error) {
  constructRejected = error instanceof TypeError;
}
function throws(expected, callback) {
  try { callback(); } catch (error) { return error instanceof expected; }
  return false;
}
var signed = new BigInt64Array([
  "18446744073709551615",
  true,
  { valueOf() { trace += "v"; return "2"; } }
]);
var failure = 0;
if (!(BigInt.name === "BigInt" && BigInt.length === 1 &&
      constructRejected && !constructCoerced)) failure = 1;
if (!(BigInt(0n) === 0n && BigInt(-1n) === -1n &&
      BigInt(9007199254740994) === 9007199254740994n &&
      BigInt(-9007199254740994) === -9007199254740994n &&
      BigInt(false) === 0n && BigInt(true) === 1n)) failure = 2;
if (!(BigInt("") === 0n && BigInt("\u00a0\u2028123\u3000") === 123n &&
      BigInt("+42") === 42n && BigInt("-42") === -42n &&
      BigInt("0b1111") === 15n && BigInt("0O70") === 56n &&
      BigInt("0xfffffffffffffffffff") === 75557863725914323419135n)) failure = 3;
if (!(BigInt(once) === 18446744073709551616n && BigInt(getter) === 1n &&
      trace === "vnumbergnumber" && thrownIdentity)) failure = 4;
if (!(signed[0] === -1n && signed[1] === 1n && signed[2] === 2n)) failure = 5;
if (!(throws(TypeError, function() { new BigInt(1); }) &&
      throws(TypeError, function() { BigInt(); }) &&
      throws(TypeError, function() { BigInt(null); }) &&
      throws(TypeError, function() { BigInt(Symbol()); }) &&
      throws(RangeError, function() { BigInt(NaN); }) &&
      throws(RangeError, function() { BigInt(Infinity); }) &&
      throws(RangeError, function() { BigInt(1.5); }) &&
      throws(SyntaxError, function() { BigInt("10n"); }) &&
      throws(SyntaxError, function() { BigInt("-0x1"); }) &&
      throws(SyntaxError, function() { BigInt("0x"); }) &&
      throws(TypeError, function() { new BigInt64Array([1]); }))) failure = 6;
failure;
"#;

const BIGINT_ARITHMETIC_SOURCE: &str = r#"
function throws(expected, callback) {
  try { callback(); } catch (error) { return error instanceof expected; }
  return false;
}
var huge = 18446744073709551616n;
var mask = 18446744073709551615n;
var trace = "";
var left = { [Symbol.toPrimitive]() { trace += "l"; return huge; } };
var right = { valueOf() { trace += "r"; return 3n; } };
var resumed = left * right;
var failure = 0;
if (!(huge + mask === 36893488147419103231n &&
      huge - mask === 1n && mask - huge === -1n &&
      huge * mask === 340282366920938463444927863358058659840n)) failure = 1;
if (!((huge * mask) / huge === mask && (huge * mask) % huge === 0n &&
      (-((huge * mask) + 1n)) % huge === -1n)) failure = 2;
if (!(3n ** 100n === 515377520732011331036461129765621272702107522001n &&
      0n ** 0n === 1n && (-1n) ** 101n === -1n)) failure = 3;
if (!((huge & mask) === 0n && (huge | mask) === 36893488147419103231n &&
      (huge ^ mask) === 36893488147419103231n && ~huge === -18446744073709551617n &&
      ~(-huge) === 18446744073709551615n)) failure = 4;
if (!((0x123456789abcdef0fedcba9876543210n << 64n) ===
        0x123456789abcdef0fedcba98765432100000000000000000n &&
      (0x123456789abcdef0fedcba9876543210n >> 64n) === 0x123456789abcdef0n &&
      (-5n >> 2n) === -2n && (-5n << -2n) === -2n &&
      (5n >> -3n) === 40n)) failure = 5;
if (!(resumed === 55340232221128654848n && trace === "lr")) failure = 6;
if (!(throws(TypeError, function() { return 1n + 1; }) &&
      throws(TypeError, function() { return 1 * 1n; }) &&
      throws(TypeError, function() { return 1n >>> 0n; }) &&
      throws(RangeError, function() { return 1n / 0n; }) &&
      throws(RangeError, function() { return 1n % 0n; }) &&
      throws(RangeError, function() { return 2n ** -1n; }))) failure = 7;
failure;
"#;

const BIGINT_WRAPPER_SOURCE: &str = r#"
function throws(expected, callback) {
  try { callback(); } catch (error) { return error instanceof expected; }
  return false;
}
var huge = 340282366920938463463374607431768211455n;
var boxed = Object(huge);
var failure = 0;
try { if (!(typeof boxed === "object" && boxed.valueOf() === huge &&
      boxed.toString() === "340282366920938463463374607431768211455" &&
      Object.prototype.toString.call(boxed) === "[object BigInt]")) failure = 1; }
catch (error) { failure = 11; }
try { if (!(huge.toString(16) === "ffffffffffffffffffffffffffffffff" &&
      (-255n).toString(16) === "-ff" && 35n.toString(36) === "z" &&
      huge.toLocaleString() === huge.toString())) failure = 2; }
catch (error) { failure = 12; }
try { if (!(BigInt.asUintN(0, -1n) === 0n && BigInt.asUintN(8, -1n) === 255n &&
      BigInt.asUintN(64, -2n) === 18446744073709551614n &&
      BigInt.asUintN(128, -1n) === huge &&
      BigInt.asIntN(8, 128n) === -128n && BigInt.asIntN(8, 127n) === 127n &&
      BigInt.asIntN(128, huge) === -1n)) failure = 3; }
catch (error) { failure = 13; }
try { if (BigInt.prototype.valueOf.call(1n) !== 1n) failure = 41;
      else if (!throws(TypeError, function() { BigInt.prototype.valueOf.call(1); })) failure = 42;
      else { var radixError = 0; try { 1n.toString(1); } catch (error) {
        radixError = error instanceof RangeError ? 1 : error instanceof TypeError ? 2 : 3;
      } if (radixError !== 1) failure = 430 + radixError; }
      if (failure === 0 && !throws(RangeError, function() { BigInt.asUintN(-1, 0n); })) failure = 44; }
catch (error) { failure = 14; }
if (!(BigInt.asIntN.length === 2 && BigInt.asUintN.length === 2 &&
      BigInt.prototype.toString.length === 0 && BigInt.prototype.valueOf.length === 0 &&
      BigInt.prototype.constructor === BigInt)) failure = 5;
failure;
"#;

#[test]
fn bigint_primitives_execute_for_every_dispatch_batch() {
    assert_bigint_source::<1>(false);
    assert_bigint_source::<2>(false);
    assert_bigint_source::<4>(false);
    assert_bigint_source::<8>(false);
    assert_bigint_source::<16>(false);
}

#[test]
fn rooted_bigint_constants_survive_forced_major_collection() {
    assert_bigint_source::<1>(true);
    assert_bigint_source::<2>(true);
    assert_bigint_source::<4>(true);
    assert_bigint_source::<8>(true);
    assert_bigint_source::<16>(true);
}

#[test]
fn bigint_constructor_and_tobigint_execute_for_every_dispatch_batch() {
    assert_bigint_conversion::<1>(false);
    assert_bigint_conversion::<2>(false);
    assert_bigint_conversion::<4>(false);
    assert_bigint_conversion::<8>(false);
    assert_bigint_conversion::<16>(false);
}

#[test]
fn bigint_conversion_callbacks_survive_forced_major_collection() {
    assert_bigint_conversion::<1>(true);
    assert_bigint_conversion::<2>(true);
    assert_bigint_conversion::<4>(true);
    assert_bigint_conversion::<8>(true);
    assert_bigint_conversion::<16>(true);
}

#[test]
fn bigint_arithmetic_executes_for_every_dispatch_batch() {
    assert_bigint_arithmetic::<1>(false);
    assert_bigint_arithmetic::<2>(false);
    assert_bigint_arithmetic::<4>(false);
    assert_bigint_arithmetic::<8>(false);
    assert_bigint_arithmetic::<16>(false);
}

#[test]
fn bigint_arithmetic_survives_forced_major_collection() {
    assert_bigint_arithmetic::<1>(true);
    assert_bigint_arithmetic::<2>(true);
    assert_bigint_arithmetic::<4>(true);
    assert_bigint_arithmetic::<8>(true);
    assert_bigint_arithmetic::<16>(true);
}

#[test]
fn bigint_wrappers_and_fixed_width_operations_execute_for_every_dispatch_batch() {
    assert_bigint_wrapper::<1>(false);
    assert_bigint_wrapper::<2>(false);
    assert_bigint_wrapper::<4>(false);
    assert_bigint_wrapper::<8>(false);
    assert_bigint_wrapper::<16>(false);
}

#[test]
fn bigint_wrappers_and_fixed_width_operations_survive_forced_major_collection() {
    assert_bigint_wrapper::<1>(true);
    assert_bigint_wrapper::<2>(true);
    assert_bigint_wrapper::<4>(true);
    assert_bigint_wrapper::<8>(true);
    assert_bigint_wrapper::<16>(true);
}

/// Compiles and executes the primitive surface under one dispatch and collection policy.
fn assert_bigint_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_400 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-primitive-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_PRIMITIVE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt primitive fixture compiles");
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
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("BigInt primitive fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Exercises constructor-only Number conversion and shared ToBigInt under one VM policy.
fn assert_bigint_conversion<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_500 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-conversion-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_CONVERSION_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt conversion fixture compiles");
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
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("BigInt conversion fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, i32={:?}",
        match outcome {
            RunOutcome::Completed(value) => value.as_i32(),
            _ => None,
        }
    );
}

/// Exercises every BigInt arithmetic family under one dispatch and collection policy.
fn assert_bigint_arithmetic<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_600 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-arithmetic-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_ARITHMETIC_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt arithmetic fixture compiles");
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
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("BigInt arithmetic fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Exercises branded wrappers, radix formatting, and fixed-width truncation under one VM policy.
fn assert_bigint_wrapper<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_700 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-wrapper-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_WRAPPER_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt wrapper fixture compiles");
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
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("BigInt wrapper fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}, i32={:?}",
        match outcome {
            RunOutcome::Completed(value) => value.as_i32(),
            _ => None,
        }
    );
}
