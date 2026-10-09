//! WorkingDirectory grants remain captured owner/descriptor grants, not an
//! arbitrary second filesystem root. Real no-follow/CAS checks run on Unix.
#![cfg(unix)]
mod support;
use support::*;
use std::path::PathBuf;
use team_agent_contract::contract::{descriptor::*,hooks::*,plan::*,types::*};
use team_agent_contract::host::{command::*,materialize::*,process::resolve_cwd};
use team_agent_contract::orchestration::{lifecycle::*,store::*};

struct CwdPlan;
impl PlanHook for CwdPlan {
    fn plan(&self,resolved:&ResolvedLaunch)->Result<LaunchPlan,ContractError> {
        let mut plan=Fake.plan(resolved)?;
        plan.materialization[0].path=OwnedPath::new(resolved.request().paths.cwd.path.clone(),PathBuf::from(".kiro/agents/owned.json"))?;
        Ok(plan)
    }
}
struct NoCommands;
impl CommandRunner for NoCommands {fn run(&mut self,_:&CommandRequest)->CommandReceipt {panic!("no command authorized by filesystem test")}}
fn fixture()->(Sandbox,ProviderDescriptor,LaunchRequest,ResolvedLaunch,LaunchPlan) {
    let sandbox=Sandbox::new();std::fs::create_dir(&sandbox.root).unwrap();
    let cwd=sandbox.parent.join("workspace");std::fs::create_dir(&cwd).unwrap();
    let mut d=descriptor();d.workspace.config_scope=ResourceScope::WorkingDirectory;d.workspace.shared_cwd=true;d.workspace.requires_materialization_lease=true;
    let mut request=request(&sandbox.root,"worker");request.paths.cwd=resolve_cwd(&cwd).unwrap();
    let mut hooks=hooks();hooks.plan=HookBinding::Bound(&CwdPlan);
    let resolved=resolve_launch(&d,&hooks,&request,None).unwrap();let plan=CwdPlan.plan(&resolved).unwrap();
    (sandbox,d,request,resolved,plan)
}

#[test]
fn lifecycle_captures_descriptor_workspace_classes_and_teardown_uses_the_same_grant() {
    let sandbox=Sandbox::new();let mut store=sandbox.store();let mut req=request(&sandbox.root,"worker");
    let cwd=sandbox.parent.join("code");std::fs::create_dir(&cwd).unwrap();req.paths.cwd=resolve_cwd(&cwd).unwrap();
    let mut d=descriptor();d.workspace.config_scope=ResourceScope::WorkingDirectory;let mut h=hooks();h.plan=HookBinding::Bound(&CwdPlan);
    let mut host=FakeHost::default();let mut io=FakeIo::new("cwd-start");
    let operation=Lifecycle {store:&mut store,host:&mut host,io:&mut io}.startup(&Adapter {descriptor:&d,hooks:&h,catalog:None},&req,routing("worker"),OperationId::new("cwd-start").unwrap()).unwrap();
    assert_eq!(operation.outcome,Outcome::Committed);assert_eq!(operation.target.workspace_resources,vec![ResourceKind::Prompt]);
    assert_eq!(operation.resources[0].path.root(),cwd);assert_ne!(cwd,sandbox.root);
    let stop=Lifecycle {store:&mut store,host:&mut host,io:&mut io}.teardown(&operation.target.identity,OperationId::new("cwd-stop").unwrap()).unwrap();
    assert_eq!(stop.outcome,Outcome::Committed);assert_eq!(host.removed,1);assert!(stop.preserved.is_empty());
}

#[test]
fn runtime_only_descriptor_cannot_write_a_working_directory_request() {
    let (_sandbox,mut d,_request,resolved,plan)=fixture();d.workspace.config_scope=ResourceScope::RuntimeRoot;
    assert!(ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).is_err());
    assert!(!plan.materialization[0].path.path().exists());
}

#[test]
fn valid_workspace_write_is_exclusive_and_records_physical_identity() {
    let (_sandbox,d,request,resolved,plan)=fixture();let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
    let receipt=io.create_exclusive(&plan.materialization[0]).unwrap();assert!(receipt.creation_identity.is_some());assert!(receipt.exclusive);
    let before=std::fs::read(receipt.path.path()).unwrap();assert_eq!(before,plan.materialization[0].contents);
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());assert_eq!(std::fs::read(receipt.path.path()).unwrap(),before);
    assert!(remove_quiescent(&request.paths.cwd,&request.identity,&receipt).unwrap());assert!(!receipt.path.path().exists());
}

#[test]
fn unplanned_path_owner_and_contents_are_rejected_before_filesystem_effects() {
    let (_sandbox,d,_request,resolved,plan)=fixture();let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
    for change in 0..3 {
        let mut request=plan.materialization[0].clone();
        match change {0=>request.owner.instance=InstanceId::new("foreign").unwrap(),1=>request.contents.push(42),_=>request.path=OwnedPath::new(request.path.root().into(),"other.json".into()).unwrap()}
        assert!(io.create_exclusive(&request).is_err());assert!(!request.path.path().exists());
    }
}

#[test]
fn symlink_config_directory_cannot_escape_the_captured_workspace() {
    use std::os::unix::fs::symlink;
    let (sandbox,d,request,resolved,plan)=fixture();let outside=sandbox.parent.join("outside");std::fs::create_dir(&outside).unwrap();symlink(&outside,request.paths.cwd.path.join(".kiro")).unwrap();
    let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());assert!(!outside.join("agents/owned.json").exists());
}

#[test]
fn replacing_workspace_after_grant_capture_cannot_transfer_authority() {
    let (sandbox,d,request,resolved,plan)=fixture();let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
    std::fs::rename(&request.paths.cwd.path,sandbox.parent.join("old-workspace")).unwrap();std::fs::create_dir(&request.paths.cwd.path).unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());assert!(!plan.materialization[0].path.path().exists());
}

#[test]
fn dirty_replaced_and_foreign_owned_files_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    for mode in 0..3 {
        let (_sandbox,d,request,resolved,plan)=fixture();let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
        let receipt=io.create_exclusive(&plan.materialization[0]).unwrap();
        let mut owner=request.identity.clone();
        match mode {
            0=>std::fs::write(receipt.path.path(),b"user changes").unwrap(),
            1=>{let replacement=receipt.path.path().with_extension("replacement");std::fs::write(&replacement,&plan.materialization[0].contents).unwrap();std::fs::set_permissions(&replacement,std::fs::Permissions::from_mode(0o600)).unwrap();std::fs::rename(replacement,receipt.path.path()).unwrap();},
            _=>owner.instance=InstanceId::new("other").unwrap(),
        }
        assert!(!remove_quiescent(&request.paths.cwd,&owner,&receipt).unwrap());assert!(receipt.path.path().exists());
    }
}

#[test]
fn shared_writable_directory_is_not_silently_chmoded_or_adopted() {
    use std::os::unix::fs::PermissionsExt;
    let (_sandbox,d,request,resolved,plan)=fixture();let original=std::fs::metadata(&request.paths.cwd.path).unwrap().permissions();
    std::fs::set_permissions(&request.paths.cwd.path,std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut io=ScopedMaterializer::new(&d,&resolved,&plan,OperationId::new("op").unwrap(),NoCommands).unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());assert_eq!(std::fs::metadata(&request.paths.cwd.path).unwrap().permissions().mode()&0o777,0o777);
    std::fs::set_permissions(&request.paths.cwd.path,original).unwrap();
}
