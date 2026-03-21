# Crypto Pipeline Report Viewer

Open [crypto-pipeline-report.html](./crypto-pipeline-report.html) in a browser, then load:

- `crypto_pipeline_report --html-output` 生成的单文件 HTML
- `crypto_pipeline_report --json-output` 生成的 combined JSON
- or:
- `crypto_seed_summary.json`
- `crypto_autolabel_summary.json`
- `crypto_prepare_summary.json`
- `crypto_calibrate_summary.json`

The page renders locally and does not need a running backend.

`crypto_pipeline_report` 也支持：

- `--title`
- `--subtitle`
- `--notes`
- `--notes-file`
- `--tag` (repeatable)
- `--input-dir`
- `--output-dir`

这样导出的 markdown / JSON / HTML 会共用同一套报表标题、副标题、批次备注和标签。
如果 summary JSON 里包含 `by_asset_class` 或 `by_event_subtype`，viewer 也会在 seed / autolabel / prepare breakdown 里一起显示。
如果加载了 `crypto_calibrate_summary.json`，viewer 还会展示 emitted vs skipped segment coverage、skip reasons，以及按 `asset_class × horizon × event_subtype` 聚合的 top underfilled buckets，并直接显示每个桶距离 `min_samples` 还差多少样本、它目前属于 `near-threshold` 还是 `far-from-threshold`，以及最接近可校准的 3 个 `near-threshold` buckets 和建议动作（`top-up-now` / `ready-soon` / `defer`）。headline 区还会额外汇总这 3 个桶的 action counts；如果 `top-up-now` 桶达到 2 个或以上，`headline-status` 和 `near-threshold-status` 都会升级成更强的 `Urgent` 提醒，并在 headline 区附一行解释文案说明紧急原因。现在这条解释也会同步显示在 hero 顶部状态附近，并按当前状态切到对应的提醒色，同时把 `top-up-now` 数量渲染成单独的 pill；headline 里的 action counts、headline 的单桶 near-threshold 行、near-threshold 表的 `Action` 列、calibrate breakdown 里的 `Skip Reason`、merge diff rows、top underfilled bucket 的 `threshold_band`，以及 `gap_to_min_samples` 都会复用同一套 chip 语言，其中 `ready-soon` 会用单独的次提醒色，不再和普通中性色混在一起，而 `insufficient_samples` 也会在 calibrate breakdown 里被单独高亮。`crypto_pipeline_report` 生成的 markdown/JSON 现在也会带同样的 `headline_explainer`。
如果同时传了 `--notes-file` 和 `--notes`，会优先使用文件内容。
aggregate JSON 现在还会额外带一个 `ui_priority_summary`，把静态 viewer 需要的 headline / near-threshold 状态、hero badge 文案和 explainer 直接算好；同时也会带结构化的 `priority_source`、`headline_status_reason`、`top_up_now_labels` 和 `near_threshold_bucket_labels`。viewer 读到它时会优先使用这份摘要，而不是在浏览器里重复推导；hero 和 headline 区也会直接显示一行紧凑的 `Triggered by` trigger chips，并在桶过多时压成 `+N more`。`crypto_pipeline_report` 生成的 markdown 现在也会把 `ui_priority_summary` 原样写出来，方便离线文本报告和静态页对齐。calibrate summary 里的 `merge_diff_summary` 也会直接保留到 aggregate JSON 里，并在 markdown/viewer 的 calibrate breakdown 里显示 `new / updated / unchanged` override rows。
如果传了 `--input-dir`，工具会按标准文件名尝试补全：

- `crypto_seed_summary.json`
- `crypto_autolabel_summary.json`
- `crypto_prepare_summary.json`
- `crypto_calibrate_summary.json`

如果传了 `--output-dir`，工具会默认写出：

- `crypto_pipeline_report.md`
- `crypto_pipeline_report.json`
- `crypto_pipeline_report.html`

报告元数据里还会自动带上 `generated_at_utc`，方便归档和批次追踪。
