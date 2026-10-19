# HelmMeet 图标

图形沿用已确认的 `#0A3281` 小帆船。设计母版在 [docs/design/brand](../../../../docs/design/brand/helmmeet-logo.svg)。

在仓库根目录执行：

```sh
python3 scripts/prepare-brand-icons.py
```

脚本调用锁定的本地Tauri CLI，从SVG生成图标；不调用图像模型、不下载依赖。仅保留本应用需要的资源，其余平台输出留在临时目录并自动清理。

- `icon.icns`：macOS应用图标的完整多尺寸资源，蓝色帆船配白色圆角底。
- `icon.png`、32/128/256px PNG：应用默认窗口图标和小尺寸资源。
- `tray-template.png`：44px透明黑色轮廓，通过Tauri的`icon_as_template(true)`交给macOS适配菜单栏明暗外观；不在菜单栏显示带白底的应用图标。
- 同步生成前端`src/assets/helmmeet-logo.svg`和浏览器`public/favicon.svg`，与设计母版保持相同内容。

本次只替换品牌标识；界面主题、按钮颜色和页面布局后续随整体产品设计实施。更换图标后需重新构建应用，正在运行的旧进程仍持有旧资源，不能靠网页热更新替换Dock/菜单栏图标。
