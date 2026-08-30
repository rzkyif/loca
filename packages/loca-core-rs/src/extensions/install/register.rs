//! `loca.json` to registry rows: the permission flattening and the
//! no-dependencies shortcut.
//!
//! Every permission kind is enumerated explicitly: a declaration lost here is
//! a permission the user is never asked about, so a missing arm must be a
//! compile error rather than a silently dropped kind.

use crate::contract::api::{
    PermissionDecl, PermissionDeclAccess, PermissionDeclPortsItem, PermissionDeclPortsItemVariant1,
};
use crate::contract::inbound::{
    ExtensionManifest, LocaJsonToolPermissions, LocaJsonToolPermissionsNetItemPortsItem,
    ToolPlatformsItem,
};

use super::super::registry::ToolInsert;

/// Flatten one tool's per-kind permission object into the stored list.
pub fn flatten_permissions(perms: &LocaJsonToolPermissions) -> Vec<PermissionDecl> {
    let mut out = Vec::new();
    for entry in perms.net.iter().flatten() {
        out.push(PermissionDecl::Net {
            host: entry.host.clone().into(),
            ports: entry.ports.iter().map(port_decl).collect(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.read.iter().flatten() {
        out.push(PermissionDecl::Read {
            path: entry.path.clone().into(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.write.iter().flatten() {
        out.push(PermissionDecl::Write {
            path: entry.path.clone().into(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.run.iter().flatten() {
        out.push(PermissionDecl::Run {
            binary: entry.binary.clone().into(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.env.iter().flatten() {
        out.push(PermissionDecl::Env {
            key: entry.key.clone().into(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.ffi.iter().flatten() {
        out.push(PermissionDecl::Ffi {
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.sys.iter().flatten() {
        out.push(PermissionDecl::Sys {
            flag: entry.flag.clone().into(),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.memories.iter().flatten() {
        out.push(PermissionDecl::Memories {
            access: memories_access(&entry.access),
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.llm.iter().flatten() {
        out.push(PermissionDecl::Llm {
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.tts.iter().flatten() {
        out.push(PermissionDecl::Tts {
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    for entry in perms.stt.iter().flatten() {
        out.push(PermissionDecl::Stt {
            reason: entry.reason.clone().into(),
            optional: entry.optional,
        });
    }
    out
}

/// A declared port, from the `loca.json` spelling to the stored one. Mapped
/// explicitly so a new member is a compile error, not a silent wildcard.
fn port_decl(port: &LocaJsonToolPermissionsNetItemPortsItem) -> PermissionDeclPortsItem {
    match port {
        LocaJsonToolPermissionsNetItemPortsItem::Variant0(number) => {
            PermissionDeclPortsItem::Variant0(i64::try_from(number.get()).unwrap_or(i64::MAX))
        }
        LocaJsonToolPermissionsNetItemPortsItem::Variant1(_) => {
            PermissionDeclPortsItem::Variant1(PermissionDeclPortsItemVariant1::X)
        }
    }
}

/// The platform token a declared platform is stored as; compared against
/// `registry::host_platforms()` by spelling alone.
fn platform_token(platform: &ToolPlatformsItem) -> String {
    match platform {
        ToolPlatformsItem::Darwin => "darwin",
        ToolPlatformsItem::Windows => "windows",
        ToolPlatformsItem::Linux => "linux",
        ToolPlatformsItem::LinuxX11 => "linux_x11",
        ToolPlatformsItem::LinuxWayland => "linux_wayland",
    }
    .to_string()
}

fn memories_access(
    access: &crate::contract::inbound::LocaJsonToolPermissionsMemoriesItemAccess,
) -> PermissionDeclAccess {
    use crate::contract::inbound::LocaJsonToolPermissionsMemoriesItemAccess as In;
    match access {
        In::Read => PermissionDeclAccess::Read,
        In::Write => PermissionDeclAccess::Write,
    }
}

/// The tool rows a manifest declares (absent and empty `tools` are the same).
pub fn tool_rows(manifest: &ExtensionManifest) -> Vec<ToolInsert> {
    manifest
        .tools
        .iter()
        .flatten()
        .map(|tool| ToolInsert {
            name: tool.name.clone().into(),
            description: tool.description.clone().into(),
            parameters: tool.parameters.clone(),
            triggers: tool.triggers.clone().unwrap_or_default(),
            fn_export: tool.function.clone().into(),
            always_available: tool.always_available,
            platforms: tool
                .platforms
                .iter()
                .flatten()
                .map(platform_token)
                .collect(),
            required_permissions: flatten_permissions(&tool.permissions),
        })
        .collect()
}

/// `@scope/name` flattens to `@scope__name`, so it lands as one folder under
/// the extensions directory rather than creating a `@scope` parent.
pub fn flatten_npm_name(name: &str) -> String {
    name.replacen('/', "__", 1)
}

/// A local-install slug becomes a directory name, so it is limited to a safe
/// identifier charset (the download also checks the resolved path stays inside).
pub fn valid_local_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use crate::contract::api::PermissionKind;

    use super::super::super::sandbox::{permission_key, permission_kind_of};

    fn manifest(raw: &str) -> ExtensionManifest {
        serde_json::from_str(raw).expect("the fixture manifest must parse")
    }

    const EVERY_KIND: &str = r#"{
      "name": "fixture",
      "displayName": "Fixture",
      "description": "every permission kind, once",
      "tools": [{
        "name": "do-thing",
        "description": "does the thing",
        "function": "doThing",
        "parameters": { "type": "object" },
        "permissions": {
          "net": [{ "host": "api.example.com", "ports": [443], "reason": "to fetch" }],
          "read": [{ "path": "$home/notes", "reason": "to read" }],
          "write": [{ "path": "$downloads", "reason": "to write" }],
          "run": [{ "binary": "ffmpeg", "reason": "to convert" }],
          "env": [{ "key": "EDITOR", "reason": "to open" }],
          "ffi": [{ "reason": "to bind" }],
          "sys": [{ "flag": "hostname", "reason": "to identify" }],
          "memories": [{ "access": "write", "reason": "to remember" }],
          "llm": [{ "reason": "to summarise" }],
          "tts": [{ "reason": "to speak" }],
          "stt": [{ "reason": "to listen" }]
        }
      }]
    }"#;

    #[test]
    fn every_declared_permission_kind_survives_the_flattening() {
        let rows = tool_rows(&manifest(EVERY_KIND));
        assert_eq!(rows.len(), 1);
        let kinds: Vec<PermissionKind> = rows[0]
            .required_permissions
            .iter()
            .map(permission_kind_of)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PermissionKind::Net,
                PermissionKind::Read,
                PermissionKind::Write,
                PermissionKind::Run,
                PermissionKind::Env,
                PermissionKind::Ffi,
                PermissionKind::Sys,
                PermissionKind::Memories,
                PermissionKind::Llm,
                PermissionKind::Tts,
                PermissionKind::Stt,
            ],
            "a permission kind was dropped or reordered by the flattening"
        );
    }

    #[test]
    fn each_flattened_declaration_carries_the_fields_its_grant_key_is_built_from() {
        let rows = tool_rows(&manifest(EVERY_KIND));
        let keys: Vec<String> = rows[0]
            .required_permissions
            .iter()
            .map(permission_key)
            .collect();
        assert!(
            keys.iter().any(|k| k.contains("api.example.com")),
            "the net host is missing from its key: {keys:?}"
        );
        assert!(keys.iter().any(|k| k.contains("$home/notes")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains("ffmpeg")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains("EDITOR")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains("hostname")), "{keys:?}");
        // Every key is distinct, or two declarations would share a grant row.
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "two declarations share a key: {keys:?}"
        );
    }

    #[test]
    fn the_net_ports_survive_and_an_omitted_ports_list_is_not_a_port() {
        let with_ports = tool_rows(&manifest(EVERY_KIND));
        match &with_ports[0].required_permissions[0] {
            PermissionDecl::Net { host, ports, .. } => {
                assert_eq!(host, "api.example.com");
                assert_eq!(ports.len(), 1, "the declared port was dropped");
                assert!(
                    matches!(ports[0], PermissionDeclPortsItem::Variant0(443)),
                    "a declared port changed value or became the wildcard: {:?}",
                    ports[0]
                );
            }
            other => panic!("expected the net declaration first, got {other:?}"),
        }

        // A net declaration with NO ports is refused by the schema.
        assert!(
            serde_json::from_str::<ExtensionManifest>(
                r#"{"name":"f","displayName":"F","description":"d","tools":[{
                    "name":"t","description":"d","function":"f","parameters":{},
                    "permissions":{"net":[{"host":"*","reason":"to fetch"}]}}]}"#,
            )
            .is_err(),
            "a net declaration with no ports parsed"
        );
        // An EMPTY `ports` list parses (Zod's `.min(1)` is not in the generated
        // `Deserialize`, filed with F2); benign only because it grants nothing,
        // which is what this pins.
        let empty_ports: ExtensionManifest = serde_json::from_str(
            r#"{"name":"f","displayName":"F","description":"d","tools":[{
                "name":"t","description":"d","function":"f","parameters":{},
                "permissions":{"net":[{"host":"api.example.com","ports":[],"reason":"r"}]}}]}"#,
        )
        .expect("the generated contract does not enforce the min(1)");
        let decls = tool_rows(&empty_ports)[0].required_permissions.clone();
        let args = super::super::super::sandbox::flag_set_to_args(
            &super::super::super::sandbox::union_flags(
                &[super::super::super::sandbox::ToolGrants {
                    grants: decls
                        .iter()
                        .map(|decl| crate::contract::api::Grant {
                            granted_at_ms: 0,
                            permission_key: permission_key(decl),
                            permission_kind: permission_kind_of(decl),
                            state: crate::contract::api::GrantState::Granted,
                            tool_id: "t".to_string(),
                        })
                        .collect(),
                    required: decls,
                }],
                &super::super::super::sandbox::PathTemplates {
                    home: "/h".into(),
                    downloads: "/h/d".into(),
                    models: "/m".into(),
                    sessions: "/s".into(),
                    extension: "/e".into(),
                },
                &std::collections::BTreeMap::new(),
            ),
        );
        assert!(
            !args.iter().any(|arg| arg.starts_with("--allow-net")),
            "an empty ports list granted network access: {args:?}"
        );

        // The wildcard port is a member of the list rather than its absence.
        let wildcard = tool_rows(&manifest(
            r#"{"name":"f","displayName":"F","description":"d","tools":[{
                "name":"t","description":"d","function":"f","parameters":{},
                "permissions":{"net":[{"host":"*","ports":["*"],"reason":"r"}]}}]}"#,
        ));
        match &wildcard[0].required_permissions[0] {
            PermissionDecl::Net { ports, .. } => {
                assert_eq!(ports.len(), 1);
                assert!(matches!(ports[0], PermissionDeclPortsItem::Variant1(_)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_tool_declaring_no_permissions_gets_an_empty_list_rather_than_a_wildcard() {
        let rows = tool_rows(&manifest(
            r#"{"name":"f","displayName":"F","description":"d","tools":[{
                "name":"t","description":"d","function":"f","parameters":{}}]}"#,
        ));
        assert!(rows[0].required_permissions.is_empty());
        assert!(!rows[0].always_available);
        assert!(rows[0].triggers.is_empty());
        assert!(
            rows[0].platforms.is_empty(),
            "an omitted platform list must mean every platform, not none"
        );
    }

    #[test]
    fn a_manifest_with_no_tools_registers_no_tools() {
        assert!(tool_rows(&manifest(
            r#"{"name":"f","displayName":"F","description":"d"}"#
        ))
        .is_empty());
        assert!(tool_rows(&manifest(
            r#"{"name":"f","displayName":"F","description":"d","tools":[]}"#
        ))
        .is_empty());
    }

    #[test]
    fn the_memories_access_is_carried_across_and_not_defaulted() {
        for (declared, expected) in [
            ("read", PermissionDeclAccess::Read),
            ("write", PermissionDeclAccess::Write),
        ] {
            let rows = tool_rows(&manifest(&format!(
                r#"{{"name":"f","displayName":"F","description":"d","tools":[{{
                    "name":"t","description":"d","function":"f","parameters":{{}},
                    "permissions":{{"memories":[{{"access":"{declared}","reason":"r"}}]}}}}]}}"#
            )));
            match &rows[0].required_permissions[0] {
                PermissionDecl::Memories { access, .. } => assert_eq!(*access, expected),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn optional_is_carried_through_because_it_decides_whether_a_tool_is_exposed() {
        let rows = tool_rows(&manifest(
            r#"{"name":"f","displayName":"F","description":"d","tools":[{
                "name":"t","description":"d","function":"f","parameters":{},
                "permissions":{"ffi":[{"reason":"r","optional":true}],
                               "llm":[{"reason":"r"}]}}]}"#,
        ));
        match &rows[0].required_permissions[0] {
            PermissionDecl::Ffi { optional, .. } => assert_eq!(*optional, Some(true)),
            other => panic!("{other:?}"),
        }
        match &rows[0].required_permissions[1] {
            PermissionDecl::Llm { optional, .. } => assert_eq!(*optional, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_platform_tokens_are_the_ones_the_host_resolver_produces() {
        assert_eq!(platform_token(&ToolPlatformsItem::Darwin), "darwin");
        assert_eq!(platform_token(&ToolPlatformsItem::Windows), "windows");
        assert_eq!(platform_token(&ToolPlatformsItem::Linux), "linux");
        assert_eq!(platform_token(&ToolPlatformsItem::LinuxX11), "linux_x11");
        assert_eq!(
            platform_token(&ToolPlatformsItem::LinuxWayland),
            "linux_wayland"
        );

        // Whatever this host resolves to must be a token some declaration could name.
        let host = super::super::super::registry::host_platforms();
        let declarable: Vec<String> = [
            ToolPlatformsItem::Darwin,
            ToolPlatformsItem::Windows,
            ToolPlatformsItem::Linux,
            ToolPlatformsItem::LinuxX11,
            ToolPlatformsItem::LinuxWayland,
        ]
        .iter()
        .map(platform_token)
        .collect();
        for token in &host {
            assert!(
                declarable.contains(token),
                "the host resolves to {token:?}, which no `loca.json` can declare"
            );
        }
        assert!(super::super::super::registry::tool_platform_supported(
            &host, &host
        ));
    }

    #[test]
    fn a_scoped_npm_name_flattens_to_one_folder() {
        assert_eq!(flatten_npm_name("@acme/loca-thing"), "@acme__loca-thing");
        assert_eq!(flatten_npm_name("plain-thing"), "plain-thing");
        // Only the first slash.
        assert_eq!(flatten_npm_name("@a/b/c"), "@a__b/c");
    }

    #[test]
    fn a_local_slug_cannot_be_a_path() {
        for bad in [
            "",
            "..",
            ".",
            "a/b",
            "a\\b",
            "a b",
            "a.b",
            "../evil",
            &"x".repeat(65),
        ] {
            assert!(!valid_local_slug(bad), "accepted {bad:?}");
        }
        for good in ["a", "my-extension", "my_extension_2", &"x".repeat(64)] {
            assert!(valid_local_slug(good), "refused {good:?}");
        }
    }
}
