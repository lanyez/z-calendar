# -*- coding: utf-8 -*-
# 设置弹窗：背板保护（内部点击不关闭）+ 标题栏拖动 + 右下角确定按钮
p = 'src/flyout.rs'
s = open(p, encoding='utf-8').read()

# 1) Action 新增
s = s.replace('''    OpenSettings,
    CloseSettings,''',
'''    OpenSettings,
    CloseSettings,
    ConfirmSettings,
    PopupDrag,
    SettingsBg,''')

# 2) Ui 字段
s = s.replace('''    settings_open: bool,
    settings_tab: usize,''',
'''    settings_open: bool,
    settings_tab: usize,
    settings_x: f32,
    settings_y: f32,
    dragging: bool,
    drag_dx: f32,
    drag_dy: f32,''')
s = s.replace('''            settings_open: false,
            settings_tab: 0,''',
'''            settings_open: false,
            settings_tab: 0,
            settings_x: 45.0,
            settings_y: 78.0,
            dragging: false,
            drag_dx: 0.0,
            drag_dy: 0.0,''')

# 3) OpenSettings 重置为居中 + ConfirmSettings 处理
s = s.replace('''            Action::OpenSettings | Action::BottomSettings => {
                self.page = Page::Calendar;
                self.settings_open = true;
                self.settings_tab = 0;
                self.redraw();
            }''',
'''            Action::OpenSettings | Action::BottomSettings => {
                self.page = Page::Calendar;
                self.settings_open = true;
                self.settings_tab = 0;
                self.settings_x = 10.0 + (WIN_W - 20.0 - 470.0) / 2.0;
                self.settings_y = 10.0 + (WIN_H - 20.0 - 580.0) / 2.0;
                self.redraw();
            }
            Action::ConfirmSettings => {
                self.settings_open = false;
                self.redraw();
            }''')

# 4) 弹窗绘制：可变坐标 + 拖拽区 + 背板区
s = s.replace('''    fn paint_settings_popup(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let pw = 470.0;
        let ph = 580.0;
        let px = 10.0 + (WIN_W - 20.0 - pw) / 2.0;
        let py = 10.0 + (WIN_H - 20.0 - ph) / 2.0;
        // 点击遮罩关闭
        Self::hit_add(regions, 10.0, 10.0, WIN_W - 20.0, WIN_H - 20.0, Action::CloseSettings);
        p.fill_round(px, py, pw, ph, 12.0, POPUP_BG);
        p.stroke_round(px, py, pw, ph, 12.0, 1.0, BORDER);

        // 标题 + 关闭
        p.text("设置", px + 16.0, py + 10.0, 100.0, 26.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL);
        Self::hit_add(regions, px + pw - 34.0, py + 10.0, 24.0, 24.0, Action::CloseSettings);
        let hov = self.hovered(&Action::CloseSettings);
        p.text("✕", px + pw - 34.0, py + 10.0, 24.0, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if hov { RED } else { WEEK_NUM });''',
'''    fn paint_settings_popup(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let pw = 470.0;
        let ph = 580.0;
        let px = self.settings_x;
        let py = self.settings_y;
        // 点击弹窗外（遮罩）关闭
        Self::hit_add(regions, 10.0, 10.0, WIN_W - 20.0, WIN_H - 20.0, Action::CloseSettings);
        // 弹窗背板：内部空白点击不关闭
        Self::hit_add(regions, px, py, pw, ph, Action::SettingsBg);
        p.fill_round(px, py, pw, ph, 12.0, POPUP_BG);
        p.stroke_round(px, py, pw, ph, 12.0, 1.0, BORDER);

        // 标题栏（可拖动）+ 关闭
        Self::hit_add(regions, px, py, pw - 40.0, 40.0, Action::PopupDrag);
        let hov_drag = self.hovered(&Action::PopupDrag);
        p.text("设置", px + 16.0, py + 10.0, 100.0, 26.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL);
        if hov_drag {
            p.text("⠿", px + pw - 70.0, py + 10.0, 30.0, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, WEEK_NUM);
        }
        Self::hit_add(regions, px + pw - 34.0, py + 10.0, 24.0, 24.0, Action::CloseSettings);
        let hov = self.hovered(&Action::CloseSettings);
        p.text("✕", px + pw - 34.0, py + 10.0, 24.0, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if hov { RED } else { WEEK_NUM });''')

# 5) 确定按钮（两个页签尾部）
s = s.replace('''            p.text(&format!("上次更新：{}", upd), cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
            y += 22.0;
            for line in [
                "节假日数据来源：chinese-days（cdn.jsdelivr.net），",
                "包含法定节假日与调休补班，自动获取最新年份。",
            ] {
                p.text(line, cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
                y += 20.0;
            }''',
'''            p.text(&format!("上次更新：{}", upd), cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
            let btn = gdi::RectF { x: px + pw - 106.0, y: py + ph - 44.0, w: 90.0, h: 30.0 };
            Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, Action::ConfirmSettings);
            let hov = self.hovered(&Action::ConfirmSettings);
            p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE });
            p.text("确定", btn.x, btn.y, btn.w, btn.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, false, WHITE);''')
s = s.replace('''            y += 52.0;
            p.text("更改设置后立即生效，无需保存。", cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);''',
'''            y += 52.0;
            p.text("更改设置后立即生效，无需保存。", cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
            let btn = gdi::RectF { x: px + pw - 106.0, y: py + ph - 44.0, w: 90.0, h: 30.0 };
            Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, Action::ConfirmSettings);
            let hov = self.hovered(&Action::ConfirmSettings);
            p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE });
            p.text("确定", btn.x, btn.y, btn.w, btn.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, false, WHITE);''')

# 6) Action::None 不再关闭设置
s = s.replace('''            Action::None => {
                if self.menu_open {
                    self.menu_open = false;
                    self.redraw();
                } else if self.settings_open {
                    self.settings_open = false;
                    self.redraw();
                }
            }''',
'''            Action::None => {
                if self.menu_open {
                    self.menu_open = false;
                    self.redraw();
                }
            }''')

# 7) WM_LBUTTONDOWN：拖动/确认/背板
s = s.replace('''            if let Some(a) = hit {
                let mut exit = false;
                {''',
'''            if let Some(a) = hit {
                if a == Action::PopupDrag {
                    let mut guard = UI.lock().unwrap();
                    if let Some(sui) = guard.as_mut() {
                        let ui = &mut sui.0;
                        let mx = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                        let my = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                        ui.dragging = true;
                        ui.drag_dx = mx - ui.settings_x;
                        ui.drag_dy = my - ui.settings_y;
                    }
                    return 0;
                }
                if a == Action::ConfirmSettings {
                    let mut guard = UI.lock().unwrap();
                    if let Some(sui) = guard.as_mut() {
                        sui.0.settings_open = false;
                        sui.0.redraw();
                    }
                    return 0;
                }
                if a == Action::SettingsBg {
                    return 0;
                }
                let mut exit = false;
                {''')

# 8) WM_MOUSEMOVE：拖动优先
s = s.replace('''        WM_MOUSEMOVE => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.shown {''',
'''        WM_MOUSEMOVE => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.dragging {
                    let mx = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                    let my = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                    ui.settings_x = (mx - ui.drag_dx).clamp(10.0, WIN_W - 20.0 - 470.0);
                    ui.settings_y = (my - ui.drag_dy).clamp(10.0, WIN_H - 20.0 - 580.0);
                    ui.redraw();
                    return 0;
                }
                if ui.shown {''')

# 9) WM_LBUTTONUP 结束拖动
s = s.replace('''        WM_CHAR => {''',
'''        WM_LBUTTONUP => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                sui.0.dragging = false;
            }
            0
        }
        WM_CHAR => {''')

open(p, 'w', encoding='utf-8', newline='').write(s)
print("popup drag/confirm patched")
