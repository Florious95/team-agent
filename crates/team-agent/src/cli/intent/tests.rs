#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use std::cell::RefCell;

pub(super) struct Sandbox(pub PathBuf);
impl Sandbox {
    pub(super) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("ta-cli-phase1-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub(super) fn path(&self) -> &Path { &self.0 }
}
impl Drop for Sandbox { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn args(values: &[&str]) -> Vec<String> { values.iter().map(|v|v.to_string()).collect() }
pub(super) fn intent(verb:Verb)->Intent{Intent{surface:Surface::Canonical,verb,subject:Subject::Agent(AgentId::new("one")),workspace:PathBuf::from("/fixture"),team:Some("pinned".into()),session:SessionPolicy::Preserve,dry_run:false,json:true,role_file:None,patch:RoleConfigPatch::default()}}
fn plan(verb:Verb,actions:&[&str])->Plan{
    let intent=intent(verb);let scope=ResolvedScope{workspace:intent.workspace.clone(),team:intent.team.clone()};
    let steps=actions.iter().map(|action|match *action{
        "stop"=>Step::StopAgent(StopAgentArgs{agent:"one".into(),workspace:scope.workspace.clone(),team:scope.team.clone(),json:true}),
        _=>Step::StartAgent(StartAgentArgs{agent:"one".into(),workspace:scope.workspace.clone(),team:scope.team.clone(),force:false,allow_fresh:false,json:true,role_config:RoleConfigPatch::default()}),
    }).collect();
    Plan{intent,scope,steps,skipped:vec![],files:vec![],expected:BTreeMap::new()}
}
#[test]
fn registered_truth_table_keeps_physics_configuration_and_intent_separate(){
    use Observation::*;use Verb::*;
    for (verb,obs,expected) in [(Start,Live,vec![]),(Start,StoppedVerified,vec!["start"]),(Start,Unstarted,vec!["start"]),(Stop,Live,vec!["stop"]),(Stop,StoppedVerified,vec![]),(Restart,Live,vec!["stop","start"]),(Restart,StoppedVerified,vec!["start"]) ]{
        assert_eq!(seat_actions(verb,obs,false,false,false,false).unwrap(),expected);
    }
    for verb in [Start,Stop,Restart]{assert!(seat_actions(verb,Unknown,false,false,false,false).is_err());assert!(seat_actions(verb,Live,true,false,false,false).is_err());}
    assert!(seat_actions(Start,Live,false,false,true,false).is_err());
    assert!(seat_actions(Restart,Unstarted,false,false,false,false).is_err());
    assert!(seat_actions(Start,Live,true,false,false,true).unwrap().is_empty());
    assert!(seat_actions(Restart,Live,false,true,false,true).unwrap().is_empty());
    assert_eq!(seat_actions(Start,StoppedVerified,false,true,false,true).unwrap(),["start"]);
}
#[test]
fn explicit_stop_uses_existing_conservative_interpretation(){
    for value in [json!(true),Value::Null,json!("false"),json!(0)]{let row=json!({"explicit_stop":value});assert!(crate::state::projection::agent_explicit_stop(&row));}
    for row in [json!({}),json!({"explicit_stop":false})]{assert!(!crate::state::projection::agent_explicit_stop(&row));}
}
#[test]
fn restart_executes_exact_pinned_stop_start_and_preserves_original_receipts(){
    let calls=RefCell::new(Vec::new());
    let result=execute_plan(plan(Verb::Restart,&["stop","start"]),&mut|step|{calls.borrow_mut().push(step.name());Ok(CmdResult::from_json(json!({"ok":true,"start_mode":"resumed","original":17}),true))},&mut|p,s,after_stop|{assert_eq!(p.scope.team.as_deref(),Some("pinned"));assert_eq!(s.agent(),Some("one"));assert_eq!(after_stop,matches!(s,Step::StartAgent(_)));Ok(())}).unwrap();
    assert_eq!(*calls.borrow(),["stop_agent","start_agent"]);
    let CmdOutput::Json(value)=result.output else{panic!()};assert_eq!(value["steps"][1]["receipt"]["original"],17);assert_eq!(value["steps"][1]["mode"],"resumed");
}
#[test]
fn stop_failure_never_starts_and_spawn_failure_is_partial_stopped(){
    for fail_at in [0,1]{let mut calls=Vec::new();let result=execute_plan(plan(Verb::Restart,&["stop","start"]),&mut|step|{let n=calls.len();calls.push(step.name());Ok(CmdResult::from_json(json!({"ok":n!=fail_at,"error":"original refusal"}),true))},&mut|_,_,_|Ok(())).unwrap();assert_eq!(result.exit,ExitCode::Error);assert_eq!(calls.len(),fail_at+1);let CmdOutput::Json(value)=result.output else{panic!()};assert_eq!(value["status"],if fail_at==0{"failed"}else{"partial_stopped"});assert_eq!(value["steps"][fail_at]["receipt"]["error"],"original refusal");}
}
#[test]
fn owner_or_incarnation_change_interrupts_even_best_effort_start(){
    let mut calls=0;let mut checks=0;
    let result=execute_plan(plan(Verb::Start,&["start","start"]),&mut|_|{calls+=1;Ok(CmdResult::from_json(json!({"ok":true}),true))},&mut|_,_,_|{checks+=1;if checks==2{Err(refusal("owner changed"))}else{Ok(())}}).unwrap();
    assert_eq!(calls,1);assert_eq!(result.exit,ExitCode::Error);
}
#[test]
fn role_change_between_stop_and_start_refuses_without_rollback_claim(){
    let mut calls=0;
    let result=execute_plan(plan(Verb::Restart,&["stop","start"]),&mut|_|{calls+=1;Ok(CmdResult::from_json(json!({"ok":true}),true))},&mut|_,_,after_stop|if after_stop{Err(refusal("role changed"))}else{Ok(())}).unwrap();
    assert_eq!(calls,1);let CmdOutput::Json(value)=result.output else{panic!()};assert_eq!(value["status"],"partial_stopped");
}
#[test]
fn dry_run_never_calls_handler_or_execution_admission(){
    let mut plan=plan(Verb::Restart,&["stop","start"]);plan.intent.dry_run=true;
    let result=execute_plan(plan,&mut|_|panic!("dry-run executed"),&mut|_,_,_|panic!("dry-run acquired execution admission")).unwrap();
    let CmdOutput::Json(value)=result.output else{panic!()};assert_eq!(value["dry_run"],true);assert_eq!(value["steps"].as_array().unwrap().len(),2);assert!(!value["deferred"].as_array().unwrap().is_empty());
}
#[test]
fn lexical_paths_never_use_dot_or_directory_existence_as_agent_inference(){
    for id in ["reviewer","reviewer.v2","worker-1"]{assert!(!path_syntax(id));}
    for path in [".","..","./worker","/tmp/worker","a/b","a\\b","C:\\team"]{assert!(path_syntax(path));}
}
#[test]
fn grammar_is_strict_and_payload_values_are_not_controls(){
    for (schema,values) in [("intent-start",vec!["one","--force"]),("intent-start",vec!["--from"]),("intent-stop",vec!["one","two"]),("intent-stop",vec!["--session","auto"]),("intent-restart",vec!["one","--model","wrong"]),("intent-restart",vec!["--session","fresh"]),("intent-start",vec!["--workspace=missing","--workspace=other"]) ]{assert!(super::super::arguments::parse(schema,&args(&values)).is_err());}
    let checked=super::super::arguments::parse("intent-start",&args(&["one","--prompt","--help"])).unwrap();assert!(!checked.help);assert!(checked.tokens.contains(&"--prompt=--help".into()));
}
#[test]
fn frozen_legacy_restart_is_not_rerouted(){
    let cwd=std::env::temp_dir();
    for values in [vec![],vec!["--workspace", "/unread-by-bridge"],vec!["./old-path"],vec!["--allow-fresh"],vec!["--help"]]{assert!(run("restart",&args(&values),&cwd).is_none());}
    for values in [vec!["--session","auto","--help"],vec!["one","--help"]]{assert!(run("restart",&args(&values),&cwd).unwrap().is_ok());}
}
#[test]
fn canonical_help_and_invalid_flags_do_not_create_runtime(){
    let root=Sandbox::new();
    for command in ["start","stop","team","agent"]{assert_eq!(run(command,&args(&["--help"]),root.path()).unwrap().unwrap().exit,ExitCode::Ok);}
    assert!(run("start",&args(&["--workspaec", "x"]),root.path()).unwrap().is_err());
    assert!(run("stop",&args(&["./one"]),root.path()).unwrap().is_err());
    assert!(!root.path().join(".team").exists());
}
