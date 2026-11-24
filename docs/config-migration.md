# 旧版 config.ini 到新版配置的映射

来源：D:\Program Files\InputTip-main\src\data\config.ini（老大本机在用配置，241 行 / 216 个键）。
迁移原则：**只迁移实际启用的方案**；旧版未启用的功能不映射、不实现。

## 1. 已启用的提示方案：光标旁文字符号

| 旧键 | 当前值 | 新键 | 备注 |
| --- | --- | --- | --- |
| caretSymbolType | 3（文字） | symbol.mode | 3 = text；1 图片、2 色块不支持 |
| caretSymbolTextContentCN/EN/Caps/JP/KR | 漢 / En / Ca / 日 / 韩 | symbol.text.<state> | |
| caretSymbolTextBgColor* | 0xA4622D / 0x87CEFA / 0xFFD166 / 0xCCCC00 / 0x800080 | symbol.bg.<state> | |
| caretSymbolTextColor* | 未设置（默认白字） | symbol.fg.<state> | 默认 0xFFFFFF |
| caretSymbolTextFont* | 霞鹜文楷等宽 | symbol.font | 缺字体时回退微软雅黑 |
| caretSymbolTextSize* | 11 | symbol.size | 逻辑像素，渲染时乘 DPI |
| caretSymbolTextWeight* | 500 | symbol.weight | 映射 DirectWrite Medium |
| caretSymbolTextTransparent* | 100 | symbol.opacity | 语义待 P1 用截图对照确认（旧版命名与取值不一致） |
| caretSymbolTextCornerPreference / EdgeStyle | 3 / 1 | symbol.corner / symbol.border | 3 = 圆角 |
| caretSymbolOriginY | above | symbol.origin | 光标上方 |
| symbolNearCursorOffsetX / Y | 0 / -20 | symbol.offset | |
| caretSymbolHideDelay | 500 | symbol.hide_delay_ms | |

## 2. 白名单（旧版 [Window.CaretSymbol.Rule.*]，全部 trigger=show）

旧版按「进程名 +（可选）标题」逐条建规则，共 24 条。新配置先收敛为进程名列表，标题随后续规则引擎再引入：

bongo-cat.exe, Code.exe, DingTalk.exe, QQ.exe, Typora.exe, Weixin.exe, msedge.exe, POWERPNT.EXE, GitMind.exe, WINWORD.EXE, Plain Craft Launcher 2.exe, EXCEL.EXE, project-graph.exe, ONENOTE.EXE, wps.exe（3 条不同标题）, fdm.exe, chrome.exe, explorer.exe, Unity Hub.exe, Animate.exe, Illustrator.exe, Files.exe

新形态（P1 只支持进程名）：

[rules.show]
1=Code.exe
2=Typora.exe

## 3. 行为与运行参数

| 旧键 | 值 | 新键 | 说明 |
| --- | --- | --- | --- |
| launchAtStartup | 2 | autostart.enabled = true | 旧方式为启动文件夹快捷方式，新版改计划任务 |
| language | zh-CN | app.language | |
| pollInterval | 20 | app.poll_interval_ms | |
| inputMethodDetectionMode | general | ime.mode | |
| inputMethodDetectionTimeout | 200 | ime.timeout_ms | |
| inputMethodSwitchState | {LShift} | switch.hotkey | P2 |
| keepCapsLockWhenStateSwitch | 0 | switch.keep_caps | P2 |
| runCodeWithAdmin | 0 | 无 | 新版不做自动提权 |

## 4. 明确不迁移（旧版未启用或已决定不做）

| 旧键 | 值 | 理由 |
| --- | --- | --- |
| cursorActive / cursorPath* | 0 / default-* | 系统光标替换方案不做（全局副作用、老大不用） |
| cursorSymbolType | 0 | 鼠标旁符号不做 |
| overlayActive / overlayText* | 0 | 悬浮小窗不做 |
| 窗口边框相关键 | 无 | 未启用 |
| enableKeyStats | 0 | 按键统计不做 |
| exportState | 0 | 状态导出不做 |
| updateCheckInterval / silentUpdate | 1440 / 1 | 更新机制 P3 再定 |
| symbolJABActive | 0 | JAB/JetBrains 桥接不做 |
| iconRunning / iconPaused / trayTipTemplate | 默认值 | 托盘文案 P2 重新设计 |

## 5. 迁移动作（实现于 P1）

1. 读取旧 config.ini（先探测 BOM，兼容 UTF-16LE 与 UTF-8）。
2. 按上表生成新 %LOCALAPPDATA%\input-tip-ng\config.ini。
3. 旧文件不动，日志记录迁移结果；新配置已存在时不覆盖。
4. 迁移后提示对比一次视觉效果，确认颜色/字号/位置一致。
