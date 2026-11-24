# input-tip-ng

InputTip 的 Rust 重写版。沿用「读当前输入法状态 → 在光标旁给出提示 → 按窗口规则调度」的核心思路，换掉 AutoHotkey 技术栈，针对四个长期痛点重做：**开机自启不可靠、部分应用读不到中/英、设置界面粗糙、高 DPI 与图标渲染差**。

> 当前处于 P0（骨架与自启）阶段，尚未包含状态识别与提示功能。路线与验收标准见 [PLAN.md](PLAN.md)。

## 为什么重写而不是改旧版

旧版 InputTip 是 AutoHotkey v2 项目（60 个 .ahk、约 1.67 万行、随包带 AHK 运行时）。四个痛点的根因都在技术栈：

| 痛点 | 旧版根因 |
| --- | --- |
| 开机自启反复失效 | 三套机制并存（HKLM Run / 计划任务 / 启动文件夹）：HKLM 需管理员、任务靠 %TEMP% 里的 PowerShell 脚本创建、checkStartup 用字符串精确比较判定，任一处变化就误报；配置还写在 exe 旁边，目录一动全失效 |
| 很多应用读不到中/英 | 只走 ImmGetDefaultIMEWnd + WM_IME_CONTROL 一条路：第三方输入法不回消息、TSF-only 应用没有 IMM 上下文、提权窗口受 UIPI 阻断，只能靠启发式猜 |
| 配置粗糙复杂 | 216 个配置键散在 20 多个独立弹窗里，没有搜索、预设、实时预览 |
| 高 DPI / 图标糊 | 预烘焙固定像素 PNG + 关掉 AHK 自动缩放后手动乘比例，150%/200% 下必然糊、必然错位 |

换成 Rust + 官方 windows 绑定后，自启、DPI、渲染、内存占用都能从机制上解决；唯一不能根治的是「跨进程读 TSF 中/英开关」——这是 Windows 公开 API 的限制，新旧版都只能靠多路探测 + 记忆弥补。

## 当前进度（P0）

- [x] 单文件 exe、零第三方运行时依赖（仅微软官方 windows crate）
- [x] 数据目录改为 %LOCALAPPDATA%\input-tip-ng，绝不写 exe 旁边
- [x] 单实例（Local 互斥体），重复启动直接退出且不报错
- [x] 常驻进程骨架：隐藏窗口 + 消息循环 + 5 分钟心跳日志
- [x] **开机自启**：计划任务（当前用户登录触发、普通权限、不弹 UAC）+ Run 键兜底
- [x] 启动自检自愈：路径漂移、任务被外部禁用、残留双份登记都会自动修复并记日志
- [x] 命令行自检：--check-autostart 输出结构化状态，退出码 0 = 健康
- [x] P1（进行中）：状态识别（多路探测 + 按进程缓存）、caret 定位、光标旁文字牌（Direct2D + DirectWrite，逐像素 alpha、按真实 DPI 排版）、进程白名单、`--probe` / `--preview` 排障命令
- [ ] P1 待办：托盘图标与菜单、旧 config.ini 迁移命令、在老大的日常应用里逐个验证位置与命中率
- [ ] P2：托盘、设置窗口、按窗口规则、状态切换热键
- [ ] P3：打包、签名、开源

## 构建

前置：Rust stable（MSVC 目标）+ Visual Studio Build Tools。

```bash
cargo build --offline      # 本机已缓存依赖时可离线构建
cargo build --release      # 产物体积优先
```

产物：target\debug\input-tip-ng.exe / target\release\input-tip-ng.exe。

## 命令行

```
input-tip-ng                       启动常驻进程
input-tip-ng --autostart           由计划任务/注册表启动时使用（等同无参数）
input-tip-ng --install-autostart   注册开机自启（首选计划任务，失败回退 Run 键）
input-tip-ng --uninstall-autostart 移除全部自启项
input-tip-ng --check-autostart     检查自启状态并自愈（退出码 0 表示健康）
input-tip-ng --exit                通知正在运行的实例退出
input-tip-ng --version / --help
```

## 数据与日志

- 配置：%LOCALAPPDATA%\input-tip-ng\config.ini（UTF-8，INI，可直接手改）
- 日志：%LOCALAPPDATA%\input-tip-ng\logs\{app,autostart}-YYYY-MM-DD.log（单文件超 2MB 滚动）

## 开机自启怎么做的

1. **只用一个来源**：计划任务 input-tip-ng，当前用户登录触发、延迟 20 秒（等输入法就绪）、普通权限。创建走 Task Scheduler COM，不经过 schtasks.exe 或 PowerShell —— 旧版把建任务塞进 %TEMP% 的 PS 脚本，杀软、执行策略、任务计划服务任一环节都能让它静默失败。
2. **兜底 Run 键**：计划任务不可用时写 HKCU\...\Run（HKCU 写入永不需要管理员）。两种方式互斥，安装任一种都会清掉另一种，避免双启后被单实例互杀。
3. **启动即自检**：每次启动比对「任务是否启用 / 动作路径是否等于当前 exe / 是否带 --autostart / 是否有登录触发器 / 运行级别是否为普通权限」，任一不符就重建；双份登记收敛到配置指定的方式。修复动作全部落日志。
4. **不受系统「启动应用」开关影响**：那是 StartupApproved 对 Run 键/启动文件夹的开关，计划任务不在其管辖内。

### 排障顺序

1. input-tip-ng --check-autostart 看结构化状态（任务/注册表/健康度一次给全）。
2. 看 %LOCALAPPDATA%\input-tip-ng\logs\autostart-*.log，每条修复都带原因。
3. 仍不启动时，用「任务计划程序」打开 input-tip-ng 任务看「上次运行结果」；常见为杀软拦截 exe 首次运行（把 exe 加入白名单，或临时改用 Run 键方式）。
4. 移动过 exe 位置时：任务仍指向旧路径，此时手工运行一次新位置的 exe，自检会把任务改指新路径。

## 目录结构

```
src/
  main.rs        入口与命令行分发
  app.rs         常驻进程外壳（单实例 + 隐藏窗口 + 消息循环）
  autostart.rs   开机自启：计划任务 COM / Run 键 / 自检自愈
  config.rs      INI 配置读写
  log.rs         数据目录与日志
docs/
  constraints.md       硬约束（改代码前必读）
  config-migration.md  旧版 config.ini 到新配置的映射
PLAN.md          阶段计划、验收矩阵、技术决策
```

## 与旧版 InputTip 的关系

- 旧版继续保留在原目录，两版**不要同时开机自启**（旧版会全局替换系统光标，且两者都会弹提示）。
- 新版不读旧版数据目录，迁移是显式的：按 [docs/config-migration.md](docs/config-migration.md) 把在用的配置项映射过来。
- 新版只重做老大实际在用的功能（见 PLAN 的实现范围），不做 1:1 复刻。

## 许可证

AGPL-3.0-only。参考自 abgox/InputTip 的产品行为与配置语义。
