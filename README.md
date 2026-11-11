# MowerHUD

MowerHUD 显示 Mower 的运行状态、当前和下一项任务及倒计时。

普通上班、下班及包含这些动作的任务标题统一显示为“基建换班”。用尽下班按任务类型单独显示；接口明确给出的回满描述、恢复原班、替班交接和轮休协调不会归并为普通换班。普通“上班”类型本身不能证明已经回满，HUD 不据此推断回满状态。

`display_name` 中的恢复原班、替班交接和轮休协调优先于 `presentation.title` 中的详细动作标题；只有未给出这些阶段时，才按展示标题和任务类型继续分类。

当前和下一任务的标题均保持单行，过长时显示省略号，悬停可查看未截断的名称。跑单房间、执行时间及其他任务名称沿用原显示规则。Mower 的完整日志和任务安排不受影响。

本机发现继续使用已有 `/mowerhud` 服务，手动连接继续使用 `/status` 和 `/task`。这项显示修复不要求 Mower 增加接口，也不改变连接配置。

## 定向验证

```sh
cargo test --locked api::tests::
cargo test --locked ui::task_view::tests::
cargo test --locked discovery::tests::
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```
