# PLAN

## 1. 目标与非目标

**目标**

1. 把老大本机在用的 InputTip 功能在 Rust 里做扎实，日常主要使用新版。
2. 四个痛点必须从机制上解决：开机自启可靠、尽可能多的应用能读到中/英、设置界面干净、高 DPI 下图像与文字锐利。
3. 单文件便携 exe、零第三方运行时依赖、低内存（目标常驻 < 15 MB）。
4. 与旧版配置语义兼容，迁移不丢用户设置。

**非目标（明确不做）**

- 不做 1:1 功能复刻（旧版 216 个配置键、5 种提示方案、JAB/JetBrains 桥接）。
- 不做系统鼠标光标替换方案（老大不用；SetSystemCursor 是全局副作用，与多应用兼容性冲突）。
- 不做鼠标旁符号、悬浮小窗、窗口边框（老大当前均未启用）。
- P0/P1 不引入 UI 框架（egui/Tauri 等）：先把手感与兼容性做对，界面 P2 用原生绘制。

## 2. 老大当前在用的功能（据 live config.ini 实测）

| 项 | 实际值 | 说明 |
| --- | --- | --- |
| 提示方案 | caretSymbolType=3（文字符号） | 光标旁文字牌，非图片 |
| 各状态文字 | 漢 / En / Ca / 日 / 韩 | 字号 11，字重 500，字体「霞鹜文楷等宽」 |
| 各状态底色 | 0xA4622D / 0x87CEFA / 0xFFD166 / 0xCCCC00 / 0x800080 | 中文棕、英文浅蓝、Caps 黄 |
| 出现位置 | caretSymbolOriginY=above，偏移 (0, -20) | 光标上方 |
| 自动隐藏 | caretSymbolHideDelay=500 ms | |
| 白名单 | 24 条 [Window.CaretSymbol.Rule.*]，全部 trigger=show | 仅在这些进程显示 |
| 已关闭 | 系统光标方案、鼠标旁符号、悬浮小窗、按键统计、状态导出 | 新版不实现 |
| 状态切换热键 | inputMethodSwitchState={LShift}、keepCapsLockWhenStateSwitch=0 | P2 实现 |
| 自启 | launchAtStartup=2（启动文件夹快捷方式） | 新版改用计划任务 |
| 语言 | zh-CN | |
| 检测 | inputMethodDetectionMode=general、超时 200ms、轮询 20ms | |

## 3. 阶段划分

### P0 骨架与开机自启（已完成 2026-11-24）

交付：项目骨架、数据目录、单实例、常驻骨架、自启三件套（安装/卸载/自检自愈）、结构化 CLI、日志。
验收：见第 4 节矩阵，11 项全部通过。

### P1 状态识别 + 光标旁文字符号（进行中，2026-11-24 起）

已完成：

- ime：ImmGetConversionStatus（跨进程）优先，WM_IME_CONTROL 兜底，键盘布局判 JP/KR/US，Caps 直接 GetKeyState；按进程缓存最后已知状态。
- caret：GetGUIThreadInfo 取前台线程 caret 矩形并转屏幕坐标。
- symbol：Direct2D + DirectWrite 离屏渲染到 32 位 DIB（预乘 alpha）→ UpdateLayeredWindow；
  灰度抗锯齿（透明底上不用次像素），按墨迹盒居中，按 GetDpiForWindow 以设备像素排版。
- 进程：QueryFullProcessImageNameW 取 exe 名，按配置白名单过滤。
- 进程级 DPI 感知：SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)，坐标与像素 1:1（旧版糊的主因）。
- 排障：--probe 打印多路探测原始值、caret；--preview 渲染一张文字牌并可导出 BMP 核对。

已完成（第二轮）：

- 托盘图标 + 右键菜单：暂停/恢复提示、打开配置、打开日志目录、退出；资源管理器重启后自动补回图标。
- --migrate：解析旧版 UTF-16 配置，迁移六状态文本/底色、字体字号字重透明度、偏移、隐藏延迟、22 条白名单、切换热键；迁移前自动备份 config.ini.bak。
- --probe 增加 GUITHREADINFO 原始字段，用于区分「系统不给 caret」与「取错线程」。

待办：

- 在有真实 caret 的应用里逐个验证位置与命中率（需老大手工验证）。
- 在老大常用应用里逐个验证：位置偏移、命中率、必须补的 per-app offset 覆盖表。

原始计划：

1. ime：多路探测（ImmGetContext + ImmGetConversionStatus → WM_IME_CONTROL → GetKeyboardLayout），按进程缓存最后已知状态，探测失败用缓存而不是猜默认值；Caps 直接 GetKeyState。
2. caret：GetGUIThreadInfo 取光标矩形；UIA TextPattern2 兜底（Chromium / 终端）。
3. symbol：分层窗口（WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW）+ DirectWrite 绘制文字牌，按 GetDpiForWindow 以设备像素渲染；圆角、底色、字色、字号按旧配置映射。
4. 白名单：按进程名决定显示与否（标题/类名规则 P2 再做）。
5. 验收：老大日常 10 个应用（VSCode、Typora、Edge、微信、QQ、Word/PowerPoint/WPS、OneNote、DingTalk、explorer）逐一切换中英，符号位置正确、无残留、无闪烁；150% 缩放下文字锐利。

### P2 托盘、设置窗口、规则与切换

1. 托盘图标 + 右键菜单（启用/暂停、设置、退出、查看日志）。
2. 单一设置窗口：左导航（常规 / 提示 / 规则 / 关于），改动即时生效，配置热重载。
3. 规则引擎：进程名 / 标题正则 / 窗口类名，show / hide 两类规则，导入导出。
4. 状态切换：全局热键（默认 LShift 单键）切中英、切键盘布局。

### P3 打包与开源

1. 单文件发布构建、图标与版本资源、可选安装器。
2. 代码签名（减少 SmartScreen / 杀软误报）。
3. 开源仓库、Issue 模板、发布流程。

## 4. 自启验收矩阵（2026-11-24 实测）

| # | 场景 | 期望 | 结果 |
| --- | --- | --- | --- |
| 1 | 首次 --install-autostart | 创建计划任务，指向当前 exe，带 --autostart | 通过 |
| 2 | 任务 XML 检查 | 登录触发、延迟 PT20S、Limited 权限、Interactive、IgnoreNew、无执行时限 | 通过 |
| 3 | schtasks /run 手动触发 | 进程启动，日志有「启动（自启）」与「常驻进程已就绪」 | 通过 |
| 4 | 任务动作被改成 notepad.exe | --check-autostart 检出路径漂移并重建 | 通过 |
| 5 | 任务被外部禁用 | --check-autostart 重新启用 | 通过 |
| 6 | 配置切到 run 方式 | 写 HKCU Run、删除计划任务 | 通过 |
| 7 | 配置切回 task 方式 | 建计划任务、删除 Run 键 | 通过 |
| 8 | 手工注入重复 Run 键 | 检出双份登记并清理 | 通过 |
| 9 | --uninstall-autostart | 任务与 Run 键全部清除 | 通过 |
| 10 | 已运行实例再次启动 | 单实例生效，正常退出、不报错 | 通过 |
| 11 | --exit | 运行实例收到 WM_CLOSE 后退出 | 通过 |

## 5. 技术决策记录

- D1 自启以计划任务为主而非 Run 键：Run 键会被「任务管理器 → 启动应用」禁用且难以感知；计划任务可读状态、可设延迟、失败可重启。
- D2 任务用 COM 创建，不调用 schtasks/PowerShell：去掉对脚本宿主、执行策略、杀软白名单的依赖。
- D3 运行级别用 Limited：旧版 Highest 依赖 UAC 与任务可用性，反而降低自启成功率；提权窗口另做「提升助手」。
- D4 数据放 %LOCALAPPDATA%：旧版写在 exe 旁边，装到 Program Files 或移动目录即失效。
- D5 零第三方依赖：只用官方 windows 绑定，离线可构建、产物体积小。
- D6 配置继续用 INI：与旧版形态一致、可手改、迁移成本低。
- D7 中文注释与中文文档：单人维护，降低认知负担。

## 6. 风险与对策

| 风险 | 对策 |
| --- | --- |
| 跨进程读 TSF 中/英本就不可能 100% | 多路探测 + 按进程记忆 + 提供「手动覆盖」；文档写明边界，不承诺全应用 |
| 杀软对未签名 exe 首次运行拦截 | P3 做签名；自启失败日志明确提示「检查安全软件」 |
| 白名单 24 条规则迁移错 | 用真实 config.ini 生成对照用例逐条比对（P2） |
| 提示符号在部分应用位置偏移 | 先做光标矩形路径，再按应用建立 offset 覆盖表（复用旧版 app-offset 思路） |
| 与旧版同时自启互相干扰 | 新版安装自启时提示；迁移完成后关闭旧版自启 |

## 7. 迁移

见 docs/config-migration.md：旧 config.ini 的键到新配置的映射、24 条白名单、丢弃项与理由。
