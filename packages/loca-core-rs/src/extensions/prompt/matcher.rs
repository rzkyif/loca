//! Decide what to do with a runtime permission prompt: auto-deny it, or forward
//! it to the user in chat. A pure function over the tool's declared permissions,
//! its grants, and the extension's undeclared-access policy.
//!
//! `granted` permissions are baked into the spawn flags, so they never prompt;
//! `ask` (or no grant row) forwards; `denied` auto-rejects; an undeclared prompt
//! follows the extension policy. A prompt that DOES arrive for a `granted`
//! declaration still forwards: the access fell outside the granted flag's
//! scope, and answering yes would silently widen the grant.

use crate::contract::api::{Grant, GrantState, PermissionDecl, PermissionKind, UndeclaredPolicy};

use crate::extensions::sandbox::{
    expand_path, permission_key, permission_kind_of, port_str, EnvLookup, PathTemplates,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAction {
    Deny,
    Forward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDecision {
    pub action: PromptAction,
    /// True when the prompt matched one of the tool's declared permissions.
    pub declared: bool,
    /// The matched declaration's reason, for the chat prompt UI.
    pub reason: Option<String>,
    pub permission_kind: PermissionKind,
}

pub struct PromptContext<'a> {
    pub required: &'a [PermissionDecl],
    pub grants: &'a [Grant],
    pub undeclared_policy: UndeclaredPolicy,
    pub templates: &'a PathTemplates,
    pub env: &'a dyn EnvLookup,
}

/// The owned form a worker carries for its lifetime, lending a [`PromptContext`] per prompt.
#[derive(Debug, Clone)]
pub struct PromptPolicy {
    pub required: Vec<PermissionDecl>,
    pub grants: Vec<Grant>,
    pub undeclared_policy: UndeclaredPolicy,
    pub templates: PathTemplates,
}

impl PromptPolicy {
    pub fn as_context<'a>(&'a self, env: &'a dyn EnvLookup) -> PromptContext<'a> {
        PromptContext {
            required: &self.required,
            grants: &self.grants,
            undeclared_policy: self.undeclared_policy,
            templates: &self.templates,
            env,
        }
    }
}

/// `None` means "no decision" (an unknown prompt kind cannot be declared,
/// granted, or rendered), and the caller must deny.
pub fn decide_prompt(
    permission: &str,
    resource: &str,
    ctx: &PromptContext<'_>,
) -> Option<PromptDecision> {
    let kind = sandbox_kind(permission)?;

    let Some(decl) = ctx
        .required
        .iter()
        .find(|d| permission_kind_of(d) == kind && decl_matches(d, resource, ctx))
    else {
        return Some(PromptDecision {
            action: match ctx.undeclared_policy {
                UndeclaredPolicy::Ask => PromptAction::Forward,
                UndeclaredPolicy::Deny => PromptAction::Deny,
            },
            declared: false,
            reason: None,
            permission_kind: kind,
        });
    };

    let key = permission_key(decl);
    let state = ctx
        .grants
        .iter()
        .find(|g| g.permission_key == key)
        .map_or(GrantState::Ask, |g| g.state);
    Some(PromptDecision {
        action: if state == GrantState::Denied {
            PromptAction::Deny
        } else {
            // Including `granted`; see the module header.
            PromptAction::Forward
        },
        declared: true,
        reason: Some(decl_reason(decl).to_string()),
        permission_kind: kind,
    })
}

/// The kinds a Deno sandbox prompt can carry AND `loca.json` can declare. Deno's
/// `import` prompt is undeclarable and the module-broker kinds never originate
/// from a runtime prompt, so both fail closed.
fn sandbox_kind(name: &str) -> Option<PermissionKind> {
    match name {
        "net" => Some(PermissionKind::Net),
        "read" => Some(PermissionKind::Read),
        "write" => Some(PermissionKind::Write),
        "run" => Some(PermissionKind::Run),
        "env" => Some(PermissionKind::Env),
        "ffi" => Some(PermissionKind::Ffi),
        "sys" => Some(PermissionKind::Sys),
        _ => None,
    }
}

fn decl_reason(decl: &PermissionDecl) -> &str {
    match decl {
        PermissionDecl::Net { reason, .. }
        | PermissionDecl::Read { reason, .. }
        | PermissionDecl::Write { reason, .. }
        | PermissionDecl::Run { reason, .. }
        | PermissionDecl::Env { reason, .. }
        | PermissionDecl::Ffi { reason, .. }
        | PermissionDecl::Sys { reason, .. }
        | PermissionDecl::Memories { reason, .. }
        | PermissionDecl::Llm { reason, .. }
        | PermissionDecl::Tts { reason, .. }
        | PermissionDecl::Stt { reason, .. } => reason,
    }
}

fn decl_matches(decl: &PermissionDecl, resource: &str, ctx: &PromptContext<'_>) -> bool {
    match decl {
        PermissionDecl::Net { host, ports, .. } => {
            // IPv6 hosts arrive bracketed, so the LAST colon is the port separator.
            let (declared_host, port) = match resource.rfind(':') {
                Some(sep) => (&resource[..sep], &resource[sep + 1..]),
                None => (resource, ""),
            };
            if host != "*" && host != declared_host {
                return false;
            }
            ports.iter().any(|p| {
                let p = port_str(p);
                p == "*" || p == port
            })
        }
        PermissionDecl::Read { path, .. } | PermissionDecl::Write { path, .. } => {
            // An empty expansion (unresolvable `$env.VAR`) must not match every path.
            let Some(base) = expand_path(path, ctx.templates, ctx.env) else {
                return false;
            };
            if resource == base {
                return true;
            }
            // Separator-terminated prefix, so `/tmp/foo` does not cover `/tmp/foobar`.
            let prefix = if base.ends_with('/') || base.ends_with('\\') {
                base.clone()
            } else {
                format!("{base}{}", path_sep(&base))
            };
            resource.starts_with(&prefix)
        }
        PermissionDecl::Run { binary, .. } => {
            if binary == resource {
                return true;
            }
            // Deno resolves a bare name to an absolute path at prompt time, so only
            // a bare declaration matches by basename; a pinned path matches exactly.
            if binary.contains('/') || binary.contains('\\') {
                return false;
            }
            basename(resource) == binary
        }
        PermissionDecl::Env { key, .. } => key == resource,
        PermissionDecl::Sys { flag, .. } => flag == resource,
        // The ffi declaration has no scope to compare the library path against.
        PermissionDecl::Ffi { .. } => true,
        PermissionDecl::Memories { .. }
        | PermissionDecl::Llm { .. }
        | PermissionDecl::Tts { .. }
        | PermissionDecl::Stt { .. } => false,
    }
}

fn path_sep(path: &str) -> char {
    if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    }
}

fn basename(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::contract::api::{PermissionDeclPortsItem, PermissionDeclPortsItemVariant1};

    fn templates() -> PathTemplates {
        PathTemplates {
            home: PathBuf::from("/home/u"),
            downloads: PathBuf::from("/home/u/Downloads"),
            models: PathBuf::from("/home/u/.loca/models"),
            sessions: PathBuf::from("/home/u/.loca/stable/core/sessions"),
            extension: PathBuf::from("/home/u/.loca/stable/core/extensions/demo"),
        }
    }

    struct Fixture {
        required: Vec<PermissionDecl>,
        grants: Vec<Grant>,
        policy: UndeclaredPolicy,
        templates: PathTemplates,
        env: BTreeMap<String, String>,
    }

    impl Fixture {
        fn new(required: Vec<PermissionDecl>) -> Self {
            Self {
                required,
                grants: Vec::new(),
                policy: UndeclaredPolicy::Deny,
                templates: templates(),
                env: BTreeMap::new(),
            }
        }

        fn policy(mut self, policy: UndeclaredPolicy) -> Self {
            self.policy = policy;
            self
        }

        fn grant(mut self, decl: &PermissionDecl, state: GrantState) -> Self {
            self.grants.push(Grant {
                granted_at_ms: 0,
                permission_key: permission_key(decl),
                permission_kind: permission_kind_of(decl),
                state,
                tool_id: "t1".to_string(),
            });
            self
        }

        fn decide(&self, permission: &str, resource: &str) -> Option<PromptDecision> {
            decide_prompt(
                permission,
                resource,
                &PromptContext {
                    required: &self.required,
                    grants: &self.grants,
                    undeclared_policy: self.policy,
                    templates: &self.templates,
                    env: &self.env,
                },
            )
        }
    }

    fn read(path: &str) -> PermissionDecl {
        PermissionDecl::Read {
            optional: None,
            path: path.to_string(),
            reason: "to read".to_string(),
        }
    }

    fn run(binary: &str) -> PermissionDecl {
        PermissionDecl::Run {
            binary: binary.to_string(),
            optional: None,
            reason: "to run".to_string(),
        }
    }

    fn net(host: &str, ports: &[PermissionDeclPortsItem]) -> PermissionDecl {
        PermissionDecl::Net {
            host: host.to_string(),
            optional: None,
            ports: ports.to_vec(),
            reason: "to fetch".to_string(),
        }
    }

    fn port(n: i64) -> PermissionDeclPortsItem {
        PermissionDeclPortsItem::Variant0(n)
    }

    fn any_port() -> PermissionDeclPortsItem {
        PermissionDeclPortsItem::Variant1(PermissionDeclPortsItemVariant1::X)
    }

    #[test]
    fn an_unknown_permission_kind_fails_closed() {
        // An `ask` policy must NOT rescue an undeclarable kind.
        let f = Fixture::new(vec![read("/tmp")]).policy(UndeclaredPolicy::Ask);
        for permission in ["import", "hrtime", ""] {
            assert_eq!(f.decide(permission, "/tmp/x"), None, "kind {permission}");
        }
    }

    #[test]
    fn a_module_broker_kind_is_not_a_sandbox_prompt() {
        let f = Fixture::new(vec![PermissionDecl::Llm {
            optional: None,
            reason: "r".to_string(),
        }])
        .policy(UndeclaredPolicy::Ask);
        for permission in ["memories", "llm", "tts", "stt"] {
            assert_eq!(f.decide(permission, ""), None, "kind {permission}");
        }
    }

    #[test]
    fn an_undeclared_prompt_follows_the_extension_policy() {
        let ask = Fixture::new(vec![]).policy(UndeclaredPolicy::Ask);
        let deny = Fixture::new(vec![]).policy(UndeclaredPolicy::Deny);
        let asked = ask.decide("read", "/etc/hosts").unwrap();
        assert_eq!(asked.action, PromptAction::Forward);
        assert!(!asked.declared);
        assert_eq!(asked.reason, None);
        assert_eq!(
            deny.decide("read", "/etc/hosts").unwrap().action,
            PromptAction::Deny
        );
    }

    #[test]
    fn a_prompt_for_a_granted_declaration_still_forwards() {
        let decl = read("$home");
        let f = Fixture::new(vec![decl.clone()]).grant(&decl, GrantState::Granted);
        let decision = f.decide("read", "/home/u/notes.txt").unwrap();
        assert_eq!(decision.action, PromptAction::Forward);
        assert!(decision.declared);
        assert_eq!(decision.reason.as_deref(), Some("to read"));
    }

    #[test]
    fn a_denied_declaration_auto_denies() {
        let decl = read("$home");
        let f = Fixture::new(vec![decl.clone()]).grant(&decl, GrantState::Denied);
        let decision = f.decide("read", "/home/u/notes.txt").unwrap();
        assert_eq!(decision.action, PromptAction::Deny);
        assert!(decision.declared);
    }

    #[test]
    fn a_declaration_with_no_grant_row_forwards() {
        let f = Fixture::new(vec![read("$home")]);
        let decision = f.decide("read", "/home/u/notes.txt").unwrap();
        assert_eq!(decision.action, PromptAction::Forward);
        assert!(decision.declared);
    }

    #[test]
    fn a_path_pinned_run_declaration_does_not_match_a_planted_binary() {
        let decl = run("/usr/bin/ffmpeg");
        let f = Fixture::new(vec![decl.clone()]).grant(&decl, GrantState::Granted);
        let decision = f.decide("run", "/tmp/evil/ffmpeg").unwrap();
        assert!(
            !decision.declared,
            "a planted binary must not satisfy a path-pinned declaration"
        );
        assert_eq!(decision.action, PromptAction::Deny, "the default policy");
        // Control: the pinned path itself still matches.
        assert!(f.decide("run", "/usr/bin/ffmpeg").unwrap().declared);
    }

    #[test]
    fn a_bare_command_declaration_matches_the_resolved_path() {
        let f = Fixture::new(vec![run("ffmpeg")]);
        assert!(
            f.decide("run", "/opt/homebrew/bin/ffmpeg")
                .unwrap()
                .declared
        );
        assert!(
            !f.decide("run", "/opt/homebrew/bin/ffmpeg2")
                .unwrap()
                .declared
        );
    }

    #[test]
    fn an_ipv6_prompt_resource_splits_on_the_last_colon() {
        let f = Fixture::new(vec![net("[::1]", &[port(443)])]);
        assert!(f.decide("net", "[::1]:443").unwrap().declared);
        assert!(!f.decide("net", "[::1]:80").unwrap().declared);
    }

    #[test]
    fn a_wildcard_host_or_port_matches_the_whole_axis() {
        let host = Fixture::new(vec![net("*", &[port(443)])]);
        assert!(host.decide("net", "anything.example:443").unwrap().declared);
        assert!(!host.decide("net", "anything.example:80").unwrap().declared);

        let port_any = Fixture::new(vec![net("example.com", &[any_port()])]);
        assert!(port_any.decide("net", "example.com:9999").unwrap().declared);
        assert!(
            !port_any
                .decide("net", "other.example:9999")
                .unwrap()
                .declared
        );
    }

    #[test]
    fn a_sibling_directory_does_not_match_a_read_declaration() {
        let f = Fixture::new(vec![read("/tmp/foo")]);
        assert!(f.decide("read", "/tmp/foo").unwrap().declared, "the base");
        assert!(f.decide("read", "/tmp/foo/a.txt").unwrap().declared);
        assert!(!f.decide("read", "/tmp/foobar").unwrap().declared);
        assert!(!f.decide("read", "/tmp/foobar/a.txt").unwrap().declared);
    }

    #[test]
    fn a_base_that_already_ends_in_a_separator_is_not_doubled() {
        let f = Fixture::new(vec![read("/tmp/foo/")]);
        assert!(f.decide("read", "/tmp/foo/a.txt").unwrap().declared);
        assert!(!f.decide("read", "/tmp/foobar").unwrap().declared);
    }

    #[test]
    fn an_empty_expansion_matches_nothing() {
        let f = Fixture::new(vec![read("$env.NOT_SET")]);
        for resource in ["/etc/shadow", "/home/u/notes.txt", ""] {
            assert!(
                !f.decide("read", resource).unwrap().declared,
                "resource {resource} matched an empty base"
            );
        }
    }

    #[test]
    fn a_read_declaration_does_not_answer_a_write_prompt() {
        let f = Fixture::new(vec![read("$home")]);
        assert!(f.decide("read", "/home/u/x").unwrap().declared);
        assert!(!f.decide("write", "/home/u/x").unwrap().declared);
    }

    #[test]
    fn env_and_sys_match_the_exact_name() {
        let f = Fixture::new(vec![
            PermissionDecl::Env {
                key: "PATH".to_string(),
                optional: None,
                reason: "r".to_string(),
            },
            PermissionDecl::Sys {
                flag: "hostname".to_string(),
                optional: None,
                reason: "r".to_string(),
            },
        ]);
        assert!(f.decide("env", "PATH").unwrap().declared);
        assert!(!f.decide("env", "PATHX").unwrap().declared);
        assert!(f.decide("sys", "hostname").unwrap().declared);
        assert!(!f.decide("sys", "osRelease").unwrap().declared);
    }

    #[test]
    fn ffi_matches_whatever_library_the_prompt_names() {
        let f = Fixture::new(vec![PermissionDecl::Ffi {
            optional: None,
            reason: "r".to_string(),
        }]);
        assert!(f.decide("ffi", "/usr/lib/libfoo.dylib").unwrap().declared);
        assert!(f.decide("ffi", "").unwrap().declared);
    }
}
