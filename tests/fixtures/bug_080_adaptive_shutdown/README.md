# bug-080 adaptive shutdown fixtures

Source: Mac mini 0.2.8 run `bug080-owned2-20260531-213101`, round `r1`, SHA `04da933fb7809dd4cc2aba548311e8fea25af1a5`; and Step 4 retest run `bug080-fix-20260531-220353`, round `r1`, SHA `3b45f0de1b30c9b12ed5a87a11fdd7bf179dbd79`.

The tmux and verdict files in this directory keep the real bug-080 lines from the realistic-tester capture. `r1-state-selected.json` keeps only the runtime fields needed by the public acceptance contract while preserving the real session names, leader receiver shape, and null adaptive display identifiers that triggered bug-080. `step4-r1-shutdown-selected.json` keeps the public-safe top-level shape of the Step 4 shutdown result that cleaned tmux objects but did not expose the orphan warning in the CLI result.
