use super::{
    ModuleAttribute, ModuleExportEntry, ModuleImportEntry, ModuleImportName, ModuleRequest,
    ModuleRequestId, ModuleStencil, ModuleStencilError,
};
use std::sync::Arc;

fn units(value: &str) -> Arc<[u16]> {
    value.encode_utf16().collect::<Vec<_>>().into()
}

fn request(specifier: &str) -> ModuleRequest {
    ModuleRequest {
        specifier: units(specifier),
        attributes: Arc::from([]),
    }
}

#[test]
/// Freezes each normalized table without retaining mutable compiler vectors.
fn freezes_normalized_module_tables() {
    let stencil = ModuleStencil::new(
        vec![request("dep")],
        vec![ModuleImportEntry {
            module_request: ModuleRequestId::new(0),
            import_name: ModuleImportName::Name(units("source")),
            local_name: Arc::from("local"),
        }],
        vec![ModuleExportEntry::Indirect {
            export_name: units("public"),
            module_request: ModuleRequestId::new(0),
            import_name: ModuleImportName::Name(units("source")),
        }],
        vec![Arc::from("declared")],
        true,
    )
    .expect("valid module stencil");

    assert_eq!(stencil.requested_modules()[0].specifier, units("dep"));
    assert_eq!(stencil.imports()[0].local_name.as_ref(), "local");
    assert!(stencil.has_top_level_await());
}

#[test]
/// Preserves ECMAScript strings that cannot be represented by Rust UTF-8 strings.
fn preserves_lone_surrogate_export_names() {
    let name: Arc<[u16]> = Arc::from([0xd800]);
    let stencil = ModuleStencil::new(
        Vec::new(),
        Vec::new(),
        vec![ModuleExportEntry::Local {
            export_name: name.clone(),
            local_name: Arc::from("local"),
        }],
        vec![Arc::from("local")],
        false,
    )
    .expect("UTF-16 export names are valid");

    let ModuleExportEntry::Local { export_name, .. } = &stencil.exports()[0] else {
        panic!("expected local export");
    };
    assert_eq!(export_name, &name);
}

#[test]
/// Rejects ambiguous request identities before publishing immutable metadata.
fn rejects_duplicate_request_identity_and_attribute_keys() {
    let duplicate_request = request("dep");
    assert_eq!(
        ModuleStencil::new(
            vec![duplicate_request.clone(), duplicate_request],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        ),
        Err(ModuleStencilError::DuplicateRequest {
            first: 0,
            duplicate: 1,
        })
    );

    let attribute = ModuleAttribute {
        key: units("type"),
        value: units("json"),
    };
    assert_eq!(
        ModuleStencil::new(
            vec![ModuleRequest {
                specifier: units("dep"),
                attributes: vec![attribute.clone(), attribute].into(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        ),
        Err(ModuleStencilError::DuplicateAttributeKey {
            request: 0,
            attribute: 1,
        })
    );
}

#[test]
/// Rejects cross-table corruption and duplicate public export names.
fn rejects_out_of_range_requests_and_duplicate_named_exports() {
    let out_of_range = ModuleStencil::new(
        Vec::new(),
        vec![ModuleImportEntry {
            module_request: ModuleRequestId::new(0),
            import_name: ModuleImportName::Namespace,
            local_name: Arc::from("namespace"),
        }],
        Vec::new(),
        Vec::new(),
        false,
    );
    assert_eq!(
        out_of_range,
        Err(ModuleStencilError::RequestOutOfRange {
            request: ModuleRequestId::new(0),
            count: 0,
        })
    );

    let name = units("same");
    let duplicate = ModuleStencil::new(
        Vec::new(),
        Vec::new(),
        vec![
            ModuleExportEntry::Local {
                export_name: name.clone(),
                local_name: Arc::from("one"),
            },
            ModuleExportEntry::Local {
                export_name: name.clone(),
                local_name: Arc::from("two"),
            },
        ],
        vec![Arc::from("one"), Arc::from("two")],
        false,
    );
    assert_eq!(
        duplicate,
        Err(ModuleStencilError::DuplicateExportName { name })
    );
}
