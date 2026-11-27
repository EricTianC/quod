//! Lean 4.36.0 `AuditHarness` renderer and JSON output parser for `Proof`,
//! `Refutation`, and pure-definition `DefOverlay` submissions.

use quod_core::SubmissionKind;
use serde::Deserialize;

/// Parameters controlling Lean Meta auditing for a single submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpec {
    pub kind: SubmissionKind,
    pub spec_module: String,
    /// Fully qualified Lean constant name for the target proposition (e.g. `Quod.Epoch0.smoke_stmt`).
    /// If `inline_target_prop` is `Some`, an inline `_quod_spec_target` definition is emitted instead.
    pub spec_decl: String,
    pub inline_target_prop: Option<String>,
    pub extra_imports: Vec<String>,
    pub candidate_module: String,
    pub candidate_decl: String,
}

#[derive(Debug, Deserialize)]
struct RawAuditOutput {
    ok: bool,
}

/// Renders a standalone `AuditHarness.lean` script compatible with Lean 4.36.0.
pub fn render_audit_harness(spec: &HarnessSpec) -> String {
    let mut imports = String::new();
    imports.push_str("import Lean\n");
    if !spec.spec_module.is_empty() {
        imports.push_str(&format!("import {}\n", spec.spec_module));
    }
    for imp in &spec.extra_imports {
        if !imp.is_empty() && imp != &spec.spec_module && imp != &spec.candidate_module {
            imports.push_str(&format!("import {imp}\n"));
        }
    }
    imports.push_str(&format!("import {}\n", spec.candidate_module));

    let (target_setup, target_expr_lean) = match &spec.inline_target_prop {
        Some(prop_src) => (
            format!("private def _quod_spec_target : Prop := {prop_src}\n"),
            "mkConst ``_quod_spec_target".to_string(),
        ),
        None => (String::new(), format!("mkConst ``{}", spec.spec_decl)),
    };

    let audit_body = match spec.kind {
        SubmissionKind::Proof => format!(
            r#"
  let axs ← collectAxioms ``{candidate_decl}
  for ax in axs do
    if !allowed.contains ax then
      throwError "QUOD_AUDIT_FORBIDDEN_AXIOM: {{ax}}"
  let env ← getEnv
  let some ci := env.find? ``{candidate_decl}
    | throwError "QUOD_AUDIT_MISSING_DECL: {candidate_decl}"
  let targetExpr := {target_expr_lean}
  if !(← isDefEq ci.type targetExpr) then
    throwError "QUOD_AUDIT_SIGNATURE_MISMATCH"
  IO.print "QUOD_AUDIT_JSON:"
  IO.println (Json.mkObj [("ok", Json.bool true)]).compress
"#,
            candidate_decl = spec.candidate_decl
        ),
        SubmissionKind::Refutation => format!(
            r#"
  let axs ← collectAxioms ``{candidate_decl}
  for ax in axs do
    if !allowed.contains ax then
      throwError "QUOD_AUDIT_FORBIDDEN_AXIOM: {{ax}}"
  let env ← getEnv
  let some ci := env.find? ``{candidate_decl}
    | throwError "QUOD_AUDIT_MISSING_DECL: {candidate_decl}"
  let targetExpr := {target_expr_lean}
  let negTarget := mkApp (mkConst ``Not) targetExpr
  if !(← isDefEq ci.type negTarget) then
    throwError "QUOD_AUDIT_SIGNATURE_MISMATCH"
  IO.print "QUOD_AUDIT_JSON:"
  IO.println (Json.mkObj [("ok", Json.bool true)]).compress
"#,
            candidate_decl = spec.candidate_decl
        ),
        SubmissionKind::DefOverlay => format!(
            r#"
  let env ← getEnv
  let some modIdx := env.getModuleIdx? `{candidate_module}
    | throwError "QUOD_AUDIT_MISSING_MODULE: {candidate_module}"
  let modData := env.header.moduleData[modIdx]!
  if modData.constNames.isEmpty then
    throwError "QUOD_AUDIT_EMPTY_OVERLAY: {candidate_module}"
  for constName in modData.constNames do
    let axs ← collectAxioms constName
    for ax in axs do
      if !allowed.contains ax then
        throwError "QUOD_AUDIT_FORBIDDEN_AXIOM: {{ax}}"
  IO.print "QUOD_AUDIT_JSON:"
  IO.println (Json.mkObj [("ok", Json.bool true)]).compress
"#,
            candidate_module = spec.candidate_module
        ),
    };

    format!(
        r#"{imports}
section QuodAudit

open Lean Meta

{target_setup}
#eval show MetaM Unit from do
  let allowed : Array Name := #[``propext, ``Classical.choice, ``Quot.sound]
{audit_body}
end QuodAudit
"#
    )
}

/// Extracts `QUOD_AUDIT_JSON:{"ok":true}` from Lean harness stdout.
pub fn parse_audit_json(stdout: &str) -> bool {
    for line in stdout.lines() {
        if let Some(raw) = line.strip_prefix("QUOD_AUDIT_JSON:")
            && let Ok(parsed) = serde_json::from_str::<RawAuditOutput>(raw.trim())
        {
            return parsed.ok;
        }
    }
    false
}
