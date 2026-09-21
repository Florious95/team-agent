# doctor / diagnose Initial RED 收据

- **判定**：权威 Initial RED（测试可编译并执行，属性断言失败）
- **Workflow**：Build macOS arm64 candidate
- **Run**：35636638556 — https://github.com/Florious95/team-agent/actions/runs/35636638556
- **Job**：Initial RED property suite / 106455567097
- **Source SHA**：`f654aa106120fffe80aef8d6d782a94894fdcfe7`
- **Source tree**：`63ada5384dfcf87beac7555e834558915ee879bf`
- **Ref**：`test/unify-doctor-diagnose-red-properties`
- **Runner**：`ubuntu-latest`
- **Command**：

```text
cargo test -p team-agent --locked \
  --test doctor_diagnose_unification_property_red \
  -- --test-threads=1 --nocapture
```

- **CommandExit**：`101`
- **Result**：`running 18 tests`; `4 passed; 14 failed; 0 ignored`
- **Receipt artifact**：`doctor-diagnose-initial-red/initial-red.log`
- **Downloaded log SHA256**：`c52d999251e21f35a47a40784cdb46b283dc20b2f4d2dbd7c4ebd20b1d40bb73`
- **Build job**：macOS arm64 `build` 被临时 `if: false` 跳过；本收据只证明红测执行，不证明产品构建。

通过的属性为 P4、P8、P10、P18；其余 14 项在未修改产品基线上按预期首红。完整 stdout/stderr 与失败断言保存在上述 GitHub Actions artifact；该收据不以 queued/accepted 状态代替真实退出码。
