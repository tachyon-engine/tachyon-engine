use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const SHARED_ARRAY_BUFFER_SOURCE: &str = r#"
var fixed = new SharedArrayBuffer(8);
var growable = new SharedArrayBuffer(4, { maxByteLength: 12 });
var callThrows = false;
var fixedGrowThrows = false;
var shrinkThrows = false;
var oversizedThrows = false;
try { SharedArrayBuffer(1); } catch (error) { callThrows = error instanceof TypeError; }
try { fixed.grow(8); } catch (error) { fixedGrowThrows = error instanceof TypeError; }
growable.grow(4);
growable.grow(9);
try { growable.grow(8); } catch (error) { shrinkThrows = error instanceof RangeError; }
try { growable.grow(13); } catch (error) { oversizedThrows = error instanceof RangeError; }
fixed.byteLength === 8 && fixed.maxByteLength === 8 && !fixed.growable &&
  growable.byteLength === 9 && growable.maxByteLength === 12 && growable.growable &&
  Object.getPrototypeOf(fixed) === SharedArrayBuffer.prototype &&
  SharedArrayBuffer.prototype.constructor === SharedArrayBuffer &&
  Object.prototype.toString.call(fixed) === "[object SharedArrayBuffer]" &&
  callThrows && fixedGrowThrows && shrinkThrows && oversizedThrows;
"#;

const SHARED_ARRAY_BUFFER_SLICE_SOURCE: &str = r#"
var order = "";
var options = {};
Object.defineProperty(options, "maxByteLength", {
  get: function() {
    order += "m";
    return { valueOf: function() { order += "v"; return 16; } };
  }
});
var source = new SharedArrayBuffer(10, options);
var sourceBytes = new Uint8Array(source);
for (var i = 0; i < sourceBytes.length; i++) sourceBytes[i] = 10 + i;
var speciesResult;
var holder = {};
Object.defineProperty(holder, Symbol.species, {
  get: function() {
    order += "p";
    return function(length) {
      order += "c" + length;
      return speciesResult = new SharedArrayBuffer(length);
    };
  }
});
source.constructor = holder;
var middle = source.slice(
  { valueOf: function() { order += "s"; return 2; } },
  { valueOf: function() { order += "e"; return -3; } }
);
var middleSpecies = speciesResult;
var all = source.slice(-Infinity, Infinity);
var empty = source.slice(8, 2);
var receiverThrows = false;
try { SharedArrayBuffer.prototype.slice.call(new ArrayBuffer(4), 0); }
catch (error) { receiverThrows = error instanceof TypeError; }
var middleBytes = new Uint8Array(middle);
middle === middleSpecies && middle !== source && middle.byteLength === 5 &&
  middle.maxByteLength === 5 && !middle.growable && middleBytes[0] === 12 &&
  middleBytes[4] === 16 && all.byteLength === 10 && empty.byteLength === 0 &&
  order === "mvsepc5pc10pc0" && receiverThrows;
"#;

#[test]
fn shared_array_buffer_constructor_and_growth_work_for_dispatch_batches() {
    assert_shared_array_buffer::<1>(SHARED_ARRAY_BUFFER_SOURCE, false);
    assert_shared_array_buffer::<2>(SHARED_ARRAY_BUFFER_SOURCE, false);
    assert_shared_array_buffer::<4>(SHARED_ARRAY_BUFFER_SOURCE, false);
    assert_shared_array_buffer::<8>(SHARED_ARRAY_BUFFER_SOURCE, false);
    assert_shared_array_buffer::<16>(SHARED_ARRAY_BUFFER_SOURCE, false);
}

#[test]
fn shared_array_buffer_slice_and_brand_checks_work_for_dispatch_batches() {
    assert_shared_array_buffer::<1>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, false);
    assert_shared_array_buffer::<2>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, false);
    assert_shared_array_buffer::<4>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, false);
    assert_shared_array_buffer::<8>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, false);
    assert_shared_array_buffer::<16>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, false);
}

#[test]
fn shared_array_buffer_backing_survives_forced_major_collection() {
    assert_shared_array_buffer::<8>(SHARED_ARRAY_BUFFER_SOURCE, true);
    assert_shared_array_buffer::<8>(SHARED_ARRAY_BUFFER_SLICE_SOURCE, true);
}

#[test]
fn unreachable_shared_backing_releases_its_external_memory_charge() {
    let module = compile_shared_array_buffer_source(
        "(function () { new SharedArrayBuffer(4096); })(); true;",
        7_451,
    );
    let mut isolate = test_isolate();
    let baseline = isolate.heap.external_bytes();
    let outcome = isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("temporary SharedArrayBuffer executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
    let charged = isolate.heap.external_bytes();
    assert_eq!(charged, baseline + 4_096);
    collect_major(&mut isolate);
    assert!(
        charged.saturating_sub(isolate.heap.external_bytes()) >= 4_096,
        "major collection must release the complete shared backing charge"
    );
}

#[test]
fn shared_array_buffer_handle_preserves_backing_identity_across_isolates() {
    let mut source = test_isolate();
    let prototype = source.realm.shared_array_buffer_prototype.unwrap();
    let buffer = source
        .allocate_shared_array_buffer_object(16, 16, false, prototype)
        .expect("source SharedArrayBuffer allocates");
    let handle = source
        .export_shared_array_buffer(buffer)
        .expect("source SharedArrayBuffer exports");

    let mut target = test_isolate();
    let imported = target
        .import_shared_array_buffer(handle.clone())
        .expect("target SharedArrayBuffer imports");
    let imported_handle = target
        .export_shared_array_buffer(imported)
        .expect("imported SharedArrayBuffer exports");

    assert!(Arc::ptr_eq(&handle.backing, &imported_handle.backing));
    assert_eq!(handle.backing.lock().unwrap().byte_length, 16);
}

/// Runs a complete major collection with every isolate-owned root category visible.
fn collect_major(isolate: &mut Isolate) {
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        inactive_realms: &mut isolate.inactive_realms,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
    };
    isolate
        .heap
        .collect_major(&mut roots)
        .expect("SharedArrayBuffer major collection succeeds");
}

/// Executes one SharedArrayBuffer fixture under a dispatch and collection policy.
fn assert_shared_array_buffer<const N: usize>(source: &'static str, forced_major: bool) {
    let module = compile_shared_array_buffer_source(source, 7_450 + N as u32);
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
        .expect("SharedArrayBuffer fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Compiles one SharedArrayBuffer fixture independently of runtime policy.
fn compile_shared_array_buffer_source(source: &'static str, id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(id),
                SourceName::new("shared-array-buffer-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("SharedArrayBuffer fixture compiles")
}
