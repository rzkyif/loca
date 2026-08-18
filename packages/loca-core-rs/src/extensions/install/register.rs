//! `loca.json` to registry rows: the permission flattening and the
//! no-dependencies shortcut.
//!
//! Ports `packages/loca-core/src/extensions/installer-register.ts`.
//!
//! # The flattening is where a permission becomes a grant key
//!
//! `loca.json` groups a tool's permissions by kind (`net: [...]`, `read: [...]`).
//! The registry stores them as one flat list, and
//! [`super::super::sandbox::permission_key`] derives a grant key from each
//! entry. So a declaration lost here is a permission the user is never asked
//! about, and one gained is a grant row for something the author never
//! declared. Every kind is enumerated explicitly rather than being iterated
//! generically, because a missing arm is then a compile error rather than a
//! silently dropped kind.

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

/// A declared port, from the `loca.json` spelling to the stored one.
///
/// Two generated enums for one concept, because the two schemas describe it
/// separately: the manifest's numeric member is a `NonZeroU64` and the stored
/// one is an `i64`. Mapped explicitly, so a member added to either side is a
/// compile error rather than a port silently becoming the wildcard.
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

/// The platform token a declared platform is stored as.
///
/// An explicit match rather than a serde round trip: these strings are compared
/// against the host's tokens by `registry::tool_platform_supported`, so a
/// spelling that drifts makes a tool invisible on the platform it declared.
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

/// The manifest's memories access, as the stored declaration spells it.
///
/// Two enums with the same two members, one from the `loca.json` schema and one
/// from the API schema. Mapped explicitly rather than by string, so a member
/// added to either is a compile error here.
fn memories_access(
    access: &crate::contract::inbound::LocaJsonToolPermissionsMemoriesItemAccess,
) -> PermissionDeclAccess {
    use crate::contract::inbound::LocaJsonToolPermissionsMemoriesItemAccess as In;
    match access {
        In::Read => PermissionDeclAccess::Read,
        In::Write => PermissionDeclAccess::Write,
    }
}

/// The tool rows a manifest declares.
///
/// `tools` is `Option<Vec<_>>` in the generated contract where the Zod schema
/// declares `.default([])`, so absent and empty are two spellings of the same
/// thing here (filed with F2). Flattening through `iter().flatten()` treats
/// them identically, which is the behaviour the specification has.
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

/// A local-install slug becomes a path segment under the extensions directory.
///
/// Constrained to a safe identifier charset so it cannot contain `.`, `..`, or
/// a separator. This is defence in depth rather than the only guard: the
/// download also checks that the resolved install path stays inside the
/// extensions directory.
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
        // A kind lost here is a permission the user is never asked about, and
        // the tool then hits a runtime prompt for something its manifest said
        // it needed. A kind gained is a grant row for something the author
        // never declared.
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
        // The key is what a grant row is stored under and what the sandbox
        // matches a runtime prompt against, so a field lost in the flattening
        // silently widens or narrows the grant.
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
        // And every key is distinct, or two declarations would share one grant
        // row and approving either would approve both.
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
                // The VALUE, not just the count. A numeric port turning into
                // the wildcard is a widening: `api.example.com:443` becomes
                // every port on that host, and the grant key moves with it, so
                // an approval the user gave for one port covers all of them.
                assert!(
                    matches!(ports[0], PermissionDeclPortsItem::Variant0(443)),
                    "a declared port changed value or became the wildcard: {:?}",
                    ports[0]
                );
            }
            other => panic!("expected the net declaration first, got {other:?}"),
        }

        // A net declaration with NO ports is refused by the schema
        // (`ports: z.array(portSchema).min(1)`), which is the stricter and
        // better shape: an author cannot ask for a host without saying which
        // ports, so there is no "every port" case for the sandbox to widen to.
        // Pinned here because the flattening would happily produce an empty
        // list, and an empty list is what a wildcard would look like.
        assert!(
            serde_json::from_str::<ExtensionManifest>(
                r#"{"name":"f","displayName":"F","description":"d","tools":[{
                    "name":"t","description":"d","function":"f","parameters":{},
                    "permissions":{"net":[{"host":"*","reason":"to fetch"}]}}]}"#,
            )
            .is_err(),
            "a net declaration with no ports parsed"
        );
        // An EMPTY `ports` list is a KNOWN DIVERGENCE, filed with F2: Zod's
        // `.min(1)` is not compiled into the generated `Deserialize`, so the
        // Deno core refuses this manifest and the Rust core accepts it.
        //
        // It is benign, and this is the assertion that makes it benign rather
        // than the assumption that it is: an empty list contributes NO entry to
        // the flag set, so the declaration grants nothing. The dangerous
        // reading would be "no ports means every port", and a later change to
        // the sandbox that took it would be caught here.
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
        // And the per-tool defaults the manifest omits.
        assert!(!rows[0].always_available);
        assert!(rows[0].triggers.is_empty());
        assert!(
            rows[0].platforms.is_empty(),
            "an omitted platform list must mean every platform, not none"
        );
    }

    #[test]
    fn a_manifest_with_no_tools_registers_no_tools() {
        // `tools` is `Option<Vec<_>>` where the Zod schema says `.default([])`,
        // so absent and empty must be one behaviour here.
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
        // Read and write are two different grants, and `write` covers a read
        // while the reverse does not. Defaulting either way would hand a tool
        // the wrong half of the memories API.
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
        // A tool is LLM-exposed only while no non-optional declared permission
        // is denied. Losing `optional` in the flattening turns an optional
        // permission the user declined into a tool that vanishes.
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
        // These strings are one half of a comparison whose other half is
        // `registry::host_platforms()`. Nothing connects them but the spelling,
        // so a drift on either side makes a tool invisible on exactly the
        // platform it declared, with no error and no log.
        assert_eq!(platform_token(&ToolPlatformsItem::Darwin), "darwin");
        assert_eq!(platform_token(&ToolPlatformsItem::Windows), "windows");
        assert_eq!(platform_token(&ToolPlatformsItem::Linux), "linux");
        assert_eq!(platform_token(&ToolPlatformsItem::LinuxX11), "linux_x11");
        assert_eq!(
            platform_token(&ToolPlatformsItem::LinuxWayland),
            "linux_wayland"
        );

        // And the cross-check that catches a drift on the OTHER side: whatever
        // this host resolves to must be a token some declaration could name.
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
        // `@scope/name` would otherwise create a `@scope` directory with the
        // extension nested inside it, and every path derived from the id would
        // point at the wrong place.
        assert_eq!(flatten_npm_name("@acme/loca-thing"), "@acme__loca-thing");
        assert_eq!(flatten_npm_name("plain-thing"), "plain-thing");
        // Only the FIRST slash: an npm name has at most one, and replacing all
        // of them would quietly accept a name with a path in it.
        assert_eq!(flatten_npm_name("@a/b/c"), "@a__b/c");
    }

    #[test]
    fn a_local_slug_cannot_be_a_path() {
        // The slug becomes a directory name under the extensions dir, so
        // anything separator-shaped or dot-shaped is a way out of it.
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
