use ratatui::style::Color;

/// Color theme for the TUI
#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub accent_light: Color,
    pub user_msg: Color,
    pub assistant_msg: Color,
    pub system_msg: Color,
    pub error: Color,
    pub success: Color,
    pub warning: Color,
    pub border: Color,
    pub border_focused: Color,
    pub status_bar_bg: Color,
    pub status_bar_fg: Color,
    pub input_bg: Color,
    pub input_fg: Color,
    pub code_block_bg: Color,
    pub line_number: Color,
    /// Tool call box border color
    pub tool_call_border: Color,
    /// Tool call header/text color
    pub tool_call_header: Color,
    /// Tool call input section label color
    pub tool_call_input_label: Color,
    /// Tool call output section label color
    pub tool_call_output_label: Color,
    /// Tool call box background
    pub tool_call_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Rgb(20, 20, 30),
            fg: Color::Rgb(220, 220, 230),
            accent: Color::Rgb(100, 150, 255),
            accent_light: Color::Rgb(150, 190, 255),
            user_msg: Color::Rgb(100, 200, 150),
            assistant_msg: Color::Rgb(220, 220, 230),
            system_msg: Color::Rgb(180, 180, 100),
            error: Color::Rgb(255, 80, 80),
            success: Color::Rgb(80, 220, 80),
            warning: Color::Rgb(255, 200, 80),
            border: Color::Rgb(60, 60, 80),
            border_focused: Color::Rgb(100, 150, 255),
            status_bar_bg: Color::Rgb(30, 30, 45),
            status_bar_fg: Color::Rgb(180, 180, 200),
            input_bg: Color::Rgb(25, 25, 38),
            input_fg: Color::Rgb(220, 220, 230),
            code_block_bg: Color::Rgb(30, 30, 45),
            line_number: Color::Rgb(80, 80, 100),
            tool_call_border: Color::Rgb(80, 140, 200),
            tool_call_header: Color::Rgb(130, 180, 240),
            tool_call_input_label: Color::Rgb(100, 200, 150),
            tool_call_output_label: Color::Rgb(150, 190, 255),
            tool_call_bg: Color::Rgb(25, 30, 40),
        }
    }
}

/// Alternative dark theme
pub fn dark_theme() -> Theme {
    Theme {
        bg: Color::Rgb(10, 10, 15),
        fg: Color::Rgb(200, 200, 210),
        accent: Color::Rgb(80, 140, 255),
        accent_light: Color::Rgb(130, 180, 255),
        ..Default::default()
    }
}

/// Light theme
pub fn light_theme() -> Theme {
    Theme {
        bg: Color::Rgb(245, 245, 250),
        fg: Color::Rgb(30, 30, 40),
        accent: Color::Rgb(60, 100, 220),
        accent_light: Color::Rgb(100, 140, 255),
        user_msg: Color::Rgb(30, 130, 80),
        assistant_msg: Color::Rgb(30, 30, 40),
        system_msg: Color::Rgb(150, 130, 30),
        error: Color::Rgb(220, 40, 40),
        success: Color::Rgb(30, 180, 30),
        warning: Color::Rgb(200, 150, 20),
        border: Color::Rgb(180, 180, 200),
        border_focused: Color::Rgb(60, 100, 220),
        status_bar_bg: Color::Rgb(230, 230, 240),
        status_bar_fg: Color::Rgb(80, 80, 100),
        input_bg: Color::Rgb(240, 240, 248),
        input_fg: Color::Rgb(30, 30, 40),
        code_block_bg: Color::Rgb(235, 235, 245),
        line_number: Color::Rgb(160, 160, 180),
        tool_call_border: Color::Rgb(60, 120, 200),
        tool_call_header: Color::Rgb(60, 100, 220),
        tool_call_input_label: Color::Rgb(30, 130, 80),
        tool_call_output_label: Color::Rgb(60, 100, 220),
        tool_call_bg: Color::Rgb(240, 240, 248),
    }
}
