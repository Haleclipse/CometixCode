//! Rust port of npm `figures` 6.1.0 plus Claude Code local figure constants.
//! Source reference: https://github.com/sindresorhus/figures (MIT). The
//! upstream package was cloned to `/tmp/figures` while porting this module.
//! Keep field names snake_case equivalents of upstream camelCase names.

#![allow(dead_code)]

use crate::utils::process_env::JsTruthy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FigureSet {
    /// Upstream `figures.circleQuestionMark`.
    pub circle_question_mark: &'static str,
    /// Upstream `figures.questionMarkPrefix`.
    pub question_mark_prefix: &'static str,
    /// Upstream `figures.square`.
    pub square: &'static str,
    /// Upstream `figures.squareDarkShade`.
    pub square_dark_shade: &'static str,
    /// Upstream `figures.squareMediumShade`.
    pub square_medium_shade: &'static str,
    /// Upstream `figures.squareLightShade`.
    pub square_light_shade: &'static str,
    /// Upstream `figures.squareTop`.
    pub square_top: &'static str,
    /// Upstream `figures.squareBottom`.
    pub square_bottom: &'static str,
    /// Upstream `figures.squareLeft`.
    pub square_left: &'static str,
    /// Upstream `figures.squareRight`.
    pub square_right: &'static str,
    /// Upstream `figures.squareCenter`.
    pub square_center: &'static str,
    /// Upstream `figures.bullet`.
    pub bullet: &'static str,
    /// Upstream `figures.dot`.
    pub dot: &'static str,
    /// Upstream `figures.ellipsis`.
    pub ellipsis: &'static str,
    /// Upstream `figures.pointerSmall`.
    pub pointer_small: &'static str,
    /// Upstream `figures.triangleUp`.
    pub triangle_up: &'static str,
    /// Upstream `figures.triangleUpSmall`.
    pub triangle_up_small: &'static str,
    /// Upstream `figures.triangleDown`.
    pub triangle_down: &'static str,
    /// Upstream `figures.triangleDownSmall`.
    pub triangle_down_small: &'static str,
    /// Upstream `figures.triangleLeftSmall`.
    pub triangle_left_small: &'static str,
    /// Upstream `figures.triangleRightSmall`.
    pub triangle_right_small: &'static str,
    /// Upstream `figures.home`.
    pub home: &'static str,
    /// Upstream `figures.heart`.
    pub heart: &'static str,
    /// Upstream `figures.musicNote`.
    pub music_note: &'static str,
    /// Upstream `figures.musicNoteBeamed`.
    pub music_note_beamed: &'static str,
    /// Upstream `figures.arrowUp`.
    pub arrow_up: &'static str,
    /// Upstream `figures.arrowDown`.
    pub arrow_down: &'static str,
    /// Upstream `figures.arrowLeft`.
    pub arrow_left: &'static str,
    /// Upstream `figures.arrowRight`.
    pub arrow_right: &'static str,
    /// Upstream `figures.arrowLeftRight`.
    pub arrow_left_right: &'static str,
    /// Upstream `figures.arrowUpDown`.
    pub arrow_up_down: &'static str,
    /// Upstream `figures.almostEqual`.
    pub almost_equal: &'static str,
    /// Upstream `figures.notEqual`.
    pub not_equal: &'static str,
    /// Upstream `figures.lessOrEqual`.
    pub less_or_equal: &'static str,
    /// Upstream `figures.greaterOrEqual`.
    pub greater_or_equal: &'static str,
    /// Upstream `figures.identical`.
    pub identical: &'static str,
    /// Upstream `figures.infinity`.
    pub infinity: &'static str,
    /// Upstream `figures.subscriptZero`.
    pub subscript_zero: &'static str,
    /// Upstream `figures.subscriptOne`.
    pub subscript_one: &'static str,
    /// Upstream `figures.subscriptTwo`.
    pub subscript_two: &'static str,
    /// Upstream `figures.subscriptThree`.
    pub subscript_three: &'static str,
    /// Upstream `figures.subscriptFour`.
    pub subscript_four: &'static str,
    /// Upstream `figures.subscriptFive`.
    pub subscript_five: &'static str,
    /// Upstream `figures.subscriptSix`.
    pub subscript_six: &'static str,
    /// Upstream `figures.subscriptSeven`.
    pub subscript_seven: &'static str,
    /// Upstream `figures.subscriptEight`.
    pub subscript_eight: &'static str,
    /// Upstream `figures.subscriptNine`.
    pub subscript_nine: &'static str,
    /// Upstream `figures.oneHalf`.
    pub one_half: &'static str,
    /// Upstream `figures.oneThird`.
    pub one_third: &'static str,
    /// Upstream `figures.oneQuarter`.
    pub one_quarter: &'static str,
    /// Upstream `figures.oneFifth`.
    pub one_fifth: &'static str,
    /// Upstream `figures.oneSixth`.
    pub one_sixth: &'static str,
    /// Upstream `figures.oneEighth`.
    pub one_eighth: &'static str,
    /// Upstream `figures.twoThirds`.
    pub two_thirds: &'static str,
    /// Upstream `figures.twoFifths`.
    pub two_fifths: &'static str,
    /// Upstream `figures.threeQuarters`.
    pub three_quarters: &'static str,
    /// Upstream `figures.threeFifths`.
    pub three_fifths: &'static str,
    /// Upstream `figures.threeEighths`.
    pub three_eighths: &'static str,
    /// Upstream `figures.fourFifths`.
    pub four_fifths: &'static str,
    /// Upstream `figures.fiveSixths`.
    pub five_sixths: &'static str,
    /// Upstream `figures.fiveEighths`.
    pub five_eighths: &'static str,
    /// Upstream `figures.sevenEighths`.
    pub seven_eighths: &'static str,
    /// Upstream `figures.line`.
    pub line: &'static str,
    /// Upstream `figures.lineBold`.
    pub line_bold: &'static str,
    /// Upstream `figures.lineDouble`.
    pub line_double: &'static str,
    /// Upstream `figures.lineDashed0`.
    pub line_dashed0: &'static str,
    /// Upstream `figures.lineDashed1`.
    pub line_dashed1: &'static str,
    /// Upstream `figures.lineDashed2`.
    pub line_dashed2: &'static str,
    /// Upstream `figures.lineDashed3`.
    pub line_dashed3: &'static str,
    /// Upstream `figures.lineDashed4`.
    pub line_dashed4: &'static str,
    /// Upstream `figures.lineDashed5`.
    pub line_dashed5: &'static str,
    /// Upstream `figures.lineDashed6`.
    pub line_dashed6: &'static str,
    /// Upstream `figures.lineDashed7`.
    pub line_dashed7: &'static str,
    /// Upstream `figures.lineDashed8`.
    pub line_dashed8: &'static str,
    /// Upstream `figures.lineDashed9`.
    pub line_dashed9: &'static str,
    /// Upstream `figures.lineDashed10`.
    pub line_dashed10: &'static str,
    /// Upstream `figures.lineDashed11`.
    pub line_dashed11: &'static str,
    /// Upstream `figures.lineDashed12`.
    pub line_dashed12: &'static str,
    /// Upstream `figures.lineDashed13`.
    pub line_dashed13: &'static str,
    /// Upstream `figures.lineDashed14`.
    pub line_dashed14: &'static str,
    /// Upstream `figures.lineDashed15`.
    pub line_dashed15: &'static str,
    /// Upstream `figures.lineVertical`.
    pub line_vertical: &'static str,
    /// Upstream `figures.lineVerticalBold`.
    pub line_vertical_bold: &'static str,
    /// Upstream `figures.lineVerticalDouble`.
    pub line_vertical_double: &'static str,
    /// Upstream `figures.lineVerticalDashed0`.
    pub line_vertical_dashed0: &'static str,
    /// Upstream `figures.lineVerticalDashed1`.
    pub line_vertical_dashed1: &'static str,
    /// Upstream `figures.lineVerticalDashed2`.
    pub line_vertical_dashed2: &'static str,
    /// Upstream `figures.lineVerticalDashed3`.
    pub line_vertical_dashed3: &'static str,
    /// Upstream `figures.lineVerticalDashed4`.
    pub line_vertical_dashed4: &'static str,
    /// Upstream `figures.lineVerticalDashed5`.
    pub line_vertical_dashed5: &'static str,
    /// Upstream `figures.lineVerticalDashed6`.
    pub line_vertical_dashed6: &'static str,
    /// Upstream `figures.lineVerticalDashed7`.
    pub line_vertical_dashed7: &'static str,
    /// Upstream `figures.lineVerticalDashed8`.
    pub line_vertical_dashed8: &'static str,
    /// Upstream `figures.lineVerticalDashed9`.
    pub line_vertical_dashed9: &'static str,
    /// Upstream `figures.lineVerticalDashed10`.
    pub line_vertical_dashed10: &'static str,
    /// Upstream `figures.lineVerticalDashed11`.
    pub line_vertical_dashed11: &'static str,
    /// Upstream `figures.lineDownLeft`.
    pub line_down_left: &'static str,
    /// Upstream `figures.lineDownLeftArc`.
    pub line_down_left_arc: &'static str,
    /// Upstream `figures.lineDownBoldLeftBold`.
    pub line_down_bold_left_bold: &'static str,
    /// Upstream `figures.lineDownBoldLeft`.
    pub line_down_bold_left: &'static str,
    /// Upstream `figures.lineDownLeftBold`.
    pub line_down_left_bold: &'static str,
    /// Upstream `figures.lineDownDoubleLeftDouble`.
    pub line_down_double_left_double: &'static str,
    /// Upstream `figures.lineDownDoubleLeft`.
    pub line_down_double_left: &'static str,
    /// Upstream `figures.lineDownLeftDouble`.
    pub line_down_left_double: &'static str,
    /// Upstream `figures.lineDownRight`.
    pub line_down_right: &'static str,
    /// Upstream `figures.lineDownRightArc`.
    pub line_down_right_arc: &'static str,
    /// Upstream `figures.lineDownBoldRightBold`.
    pub line_down_bold_right_bold: &'static str,
    /// Upstream `figures.lineDownBoldRight`.
    pub line_down_bold_right: &'static str,
    /// Upstream `figures.lineDownRightBold`.
    pub line_down_right_bold: &'static str,
    /// Upstream `figures.lineDownDoubleRightDouble`.
    pub line_down_double_right_double: &'static str,
    /// Upstream `figures.lineDownDoubleRight`.
    pub line_down_double_right: &'static str,
    /// Upstream `figures.lineDownRightDouble`.
    pub line_down_right_double: &'static str,
    /// Upstream `figures.lineUpLeft`.
    pub line_up_left: &'static str,
    /// Upstream `figures.lineUpLeftArc`.
    pub line_up_left_arc: &'static str,
    /// Upstream `figures.lineUpBoldLeftBold`.
    pub line_up_bold_left_bold: &'static str,
    /// Upstream `figures.lineUpBoldLeft`.
    pub line_up_bold_left: &'static str,
    /// Upstream `figures.lineUpLeftBold`.
    pub line_up_left_bold: &'static str,
    /// Upstream `figures.lineUpDoubleLeftDouble`.
    pub line_up_double_left_double: &'static str,
    /// Upstream `figures.lineUpDoubleLeft`.
    pub line_up_double_left: &'static str,
    /// Upstream `figures.lineUpLeftDouble`.
    pub line_up_left_double: &'static str,
    /// Upstream `figures.lineUpRight`.
    pub line_up_right: &'static str,
    /// Upstream `figures.lineUpRightArc`.
    pub line_up_right_arc: &'static str,
    /// Upstream `figures.lineUpBoldRightBold`.
    pub line_up_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldRight`.
    pub line_up_bold_right: &'static str,
    /// Upstream `figures.lineUpRightBold`.
    pub line_up_right_bold: &'static str,
    /// Upstream `figures.lineUpDoubleRightDouble`.
    pub line_up_double_right_double: &'static str,
    /// Upstream `figures.lineUpDoubleRight`.
    pub line_up_double_right: &'static str,
    /// Upstream `figures.lineUpRightDouble`.
    pub line_up_right_double: &'static str,
    /// Upstream `figures.lineUpDownLeft`.
    pub line_up_down_left: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeftBold`.
    pub line_up_bold_down_bold_left_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeft`.
    pub line_up_bold_down_bold_left: &'static str,
    /// Upstream `figures.lineUpDownLeftBold`.
    pub line_up_down_left_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownLeftBold`.
    pub line_up_bold_down_left_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldLeftBold`.
    pub line_up_down_bold_left_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldLeft`.
    pub line_up_down_bold_left: &'static str,
    /// Upstream `figures.lineUpBoldDownLeft`.
    pub line_up_bold_down_left: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleLeftDouble`.
    pub line_up_double_down_double_left_double: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleLeft`.
    pub line_up_double_down_double_left: &'static str,
    /// Upstream `figures.lineUpDownLeftDouble`.
    pub line_up_down_left_double: &'static str,
    /// Upstream `figures.lineUpDownRight`.
    pub line_up_down_right: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldRightBold`.
    pub line_up_bold_down_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldRight`.
    pub line_up_bold_down_bold_right: &'static str,
    /// Upstream `figures.lineUpDownRightBold`.
    pub line_up_down_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownRightBold`.
    pub line_up_bold_down_right_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldRightBold`.
    pub line_up_down_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldRight`.
    pub line_up_down_bold_right: &'static str,
    /// Upstream `figures.lineUpBoldDownRight`.
    pub line_up_bold_down_right: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleRightDouble`.
    pub line_up_double_down_double_right_double: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleRight`.
    pub line_up_double_down_double_right: &'static str,
    /// Upstream `figures.lineUpDownRightDouble`.
    pub line_up_down_right_double: &'static str,
    /// Upstream `figures.lineDownLeftRight`.
    pub line_down_left_right: &'static str,
    /// Upstream `figures.lineDownBoldLeftBoldRightBold`.
    pub line_down_bold_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineDownLeftBoldRightBold`.
    pub line_down_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineDownBoldLeftRight`.
    pub line_down_bold_left_right: &'static str,
    /// Upstream `figures.lineDownBoldLeftBoldRight`.
    pub line_down_bold_left_bold_right: &'static str,
    /// Upstream `figures.lineDownBoldLeftRightBold`.
    pub line_down_bold_left_right_bold: &'static str,
    /// Upstream `figures.lineDownLeftRightBold`.
    pub line_down_left_right_bold: &'static str,
    /// Upstream `figures.lineDownLeftBoldRight`.
    pub line_down_left_bold_right: &'static str,
    /// Upstream `figures.lineDownDoubleLeftDoubleRightDouble`.
    pub line_down_double_left_double_right_double: &'static str,
    /// Upstream `figures.lineDownDoubleLeftRight`.
    pub line_down_double_left_right: &'static str,
    /// Upstream `figures.lineDownLeftDoubleRightDouble`.
    pub line_down_left_double_right_double: &'static str,
    /// Upstream `figures.lineUpLeftRight`.
    pub line_up_left_right: &'static str,
    /// Upstream `figures.lineUpBoldLeftBoldRightBold`.
    pub line_up_bold_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpLeftBoldRightBold`.
    pub line_up_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldLeftRight`.
    pub line_up_bold_left_right: &'static str,
    /// Upstream `figures.lineUpBoldLeftBoldRight`.
    pub line_up_bold_left_bold_right: &'static str,
    /// Upstream `figures.lineUpBoldLeftRightBold`.
    pub line_up_bold_left_right_bold: &'static str,
    /// Upstream `figures.lineUpLeftRightBold`.
    pub line_up_left_right_bold: &'static str,
    /// Upstream `figures.lineUpLeftBoldRight`.
    pub line_up_left_bold_right: &'static str,
    /// Upstream `figures.lineUpDoubleLeftDoubleRightDouble`.
    pub line_up_double_left_double_right_double: &'static str,
    /// Upstream `figures.lineUpDoubleLeftRight`.
    pub line_up_double_left_right: &'static str,
    /// Upstream `figures.lineUpLeftDoubleRightDouble`.
    pub line_up_left_double_right_double: &'static str,
    /// Upstream `figures.lineUpDownLeftRight`.
    pub line_up_down_left_right: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeftBoldRightBold`.
    pub line_up_bold_down_bold_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldLeftBoldRightBold`.
    pub line_up_down_bold_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownLeftBoldRightBold`.
    pub line_up_bold_down_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeftRightBold`.
    pub line_up_bold_down_bold_left_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeftBoldRight`.
    pub line_up_bold_down_bold_left_bold_right: &'static str,
    /// Upstream `figures.lineUpBoldDownLeftRight`.
    pub line_up_bold_down_left_right: &'static str,
    /// Upstream `figures.lineUpDownBoldLeftRight`.
    pub line_up_down_bold_left_right: &'static str,
    /// Upstream `figures.lineUpDownLeftBoldRight`.
    pub line_up_down_left_bold_right: &'static str,
    /// Upstream `figures.lineUpDownLeftRightBold`.
    pub line_up_down_left_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownBoldLeftRight`.
    pub line_up_bold_down_bold_left_right: &'static str,
    /// Upstream `figures.lineUpDownLeftBoldRightBold`.
    pub line_up_down_left_bold_right_bold: &'static str,
    /// Upstream `figures.lineUpBoldDownLeftBoldRight`.
    pub line_up_bold_down_left_bold_right: &'static str,
    /// Upstream `figures.lineUpBoldDownLeftRightBold`.
    pub line_up_bold_down_left_right_bold: &'static str,
    /// Upstream `figures.lineUpDownBoldLeftBoldRight`.
    pub line_up_down_bold_left_bold_right: &'static str,
    /// Upstream `figures.lineUpDownBoldLeftRightBold`.
    pub line_up_down_bold_left_right_bold: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleLeftDoubleRightDouble`.
    pub line_up_double_down_double_left_double_right_double: &'static str,
    /// Upstream `figures.lineUpDoubleDownDoubleLeftRight`.
    pub line_up_double_down_double_left_right: &'static str,
    /// Upstream `figures.lineUpDownLeftDoubleRightDouble`.
    pub line_up_down_left_double_right_double: &'static str,
    /// Upstream `figures.lineCross`.
    pub line_cross: &'static str,
    /// Upstream `figures.lineBackslash`.
    pub line_backslash: &'static str,
    /// Upstream `figures.lineSlash`.
    pub line_slash: &'static str,
    /// Upstream `figures.tick`.
    pub tick: &'static str,
    /// Upstream `figures.info`.
    pub info: &'static str,
    /// Upstream `figures.warning`.
    pub warning: &'static str,
    /// Upstream `figures.cross`.
    pub cross: &'static str,
    /// Upstream `figures.squareSmall`.
    pub square_small: &'static str,
    /// Upstream `figures.squareSmallFilled`.
    pub square_small_filled: &'static str,
    /// Upstream `figures.circle`.
    pub circle: &'static str,
    /// Upstream `figures.circleFilled`.
    pub circle_filled: &'static str,
    /// Upstream `figures.circleDotted`.
    pub circle_dotted: &'static str,
    /// Upstream `figures.circleDouble`.
    pub circle_double: &'static str,
    /// Upstream `figures.circleCircle`.
    pub circle_circle: &'static str,
    /// Upstream `figures.circleCross`.
    pub circle_cross: &'static str,
    /// Upstream `figures.circlePipe`.
    pub circle_pipe: &'static str,
    /// Upstream `figures.radioOn`.
    pub radio_on: &'static str,
    /// Upstream `figures.radioOff`.
    pub radio_off: &'static str,
    /// Upstream `figures.checkboxOn`.
    pub checkbox_on: &'static str,
    /// Upstream `figures.checkboxOff`.
    pub checkbox_off: &'static str,
    /// Upstream `figures.checkboxCircleOn`.
    pub checkbox_circle_on: &'static str,
    /// Upstream `figures.checkboxCircleOff`.
    pub checkbox_circle_off: &'static str,
    /// Upstream `figures.pointer`.
    pub pointer: &'static str,
    /// Upstream `figures.triangleUpOutline`.
    pub triangle_up_outline: &'static str,
    /// Upstream `figures.triangleLeft`.
    pub triangle_left: &'static str,
    /// Upstream `figures.triangleRight`.
    pub triangle_right: &'static str,
    /// Upstream `figures.lozenge`.
    pub lozenge: &'static str,
    /// Upstream `figures.lozengeOutline`.
    pub lozenge_outline: &'static str,
    /// Upstream `figures.hamburger`.
    pub hamburger: &'static str,
    /// Upstream `figures.smiley`.
    pub smiley: &'static str,
    /// Upstream `figures.mustache`.
    pub mustache: &'static str,
    /// Upstream `figures.star`.
    pub star: &'static str,
    /// Upstream `figures.play`.
    pub play: &'static str,
    /// Upstream `figures.nodejs`.
    pub nodejs: &'static str,
    /// Upstream `figures.oneSeventh`.
    pub one_seventh: &'static str,
    /// Upstream `figures.oneNinth`.
    pub one_ninth: &'static str,
    /// Upstream `figures.oneTenth`.
    pub one_tenth: &'static str,
}

pub const MAIN_SYMBOLS: FigureSet = FigureSet {
    circle_question_mark: "(?)",
    question_mark_prefix: "(?)",
    square: "█",
    square_dark_shade: "▓",
    square_medium_shade: "▒",
    square_light_shade: "░",
    square_top: "▀",
    square_bottom: "▄",
    square_left: "▌",
    square_right: "▐",
    square_center: "■",
    bullet: "●",
    dot: "․",
    ellipsis: "…",
    pointer_small: "›",
    triangle_up: "▲",
    triangle_up_small: "▴",
    triangle_down: "▼",
    triangle_down_small: "▾",
    triangle_left_small: "◂",
    triangle_right_small: "▸",
    home: "⌂",
    heart: "♥",
    music_note: "♪",
    music_note_beamed: "♫",
    arrow_up: "↑",
    arrow_down: "↓",
    arrow_left: "←",
    arrow_right: "→",
    arrow_left_right: "↔",
    arrow_up_down: "↕",
    almost_equal: "≈",
    not_equal: "≠",
    less_or_equal: "≤",
    greater_or_equal: "≥",
    identical: "≡",
    infinity: "∞",
    subscript_zero: "₀",
    subscript_one: "₁",
    subscript_two: "₂",
    subscript_three: "₃",
    subscript_four: "₄",
    subscript_five: "₅",
    subscript_six: "₆",
    subscript_seven: "₇",
    subscript_eight: "₈",
    subscript_nine: "₉",
    one_half: "½",
    one_third: "⅓",
    one_quarter: "¼",
    one_fifth: "⅕",
    one_sixth: "⅙",
    one_eighth: "⅛",
    two_thirds: "⅔",
    two_fifths: "⅖",
    three_quarters: "¾",
    three_fifths: "⅗",
    three_eighths: "⅜",
    four_fifths: "⅘",
    five_sixths: "⅚",
    five_eighths: "⅝",
    seven_eighths: "⅞",
    line: "─",
    line_bold: "━",
    line_double: "═",
    line_dashed0: "┄",
    line_dashed1: "┅",
    line_dashed2: "┈",
    line_dashed3: "┉",
    line_dashed4: "╌",
    line_dashed5: "╍",
    line_dashed6: "╴",
    line_dashed7: "╶",
    line_dashed8: "╸",
    line_dashed9: "╺",
    line_dashed10: "╼",
    line_dashed11: "╾",
    line_dashed12: "−",
    line_dashed13: "–",
    line_dashed14: "‐",
    line_dashed15: "⁃",
    line_vertical: "│",
    line_vertical_bold: "┃",
    line_vertical_double: "║",
    line_vertical_dashed0: "┆",
    line_vertical_dashed1: "┇",
    line_vertical_dashed2: "┊",
    line_vertical_dashed3: "┋",
    line_vertical_dashed4: "╎",
    line_vertical_dashed5: "╏",
    line_vertical_dashed6: "╵",
    line_vertical_dashed7: "╷",
    line_vertical_dashed8: "╹",
    line_vertical_dashed9: "╻",
    line_vertical_dashed10: "╽",
    line_vertical_dashed11: "╿",
    line_down_left: "┐",
    line_down_left_arc: "╮",
    line_down_bold_left_bold: "┓",
    line_down_bold_left: "┒",
    line_down_left_bold: "┑",
    line_down_double_left_double: "╗",
    line_down_double_left: "╖",
    line_down_left_double: "╕",
    line_down_right: "┌",
    line_down_right_arc: "╭",
    line_down_bold_right_bold: "┏",
    line_down_bold_right: "┎",
    line_down_right_bold: "┍",
    line_down_double_right_double: "╔",
    line_down_double_right: "╓",
    line_down_right_double: "╒",
    line_up_left: "┘",
    line_up_left_arc: "╯",
    line_up_bold_left_bold: "┛",
    line_up_bold_left: "┚",
    line_up_left_bold: "┙",
    line_up_double_left_double: "╝",
    line_up_double_left: "╜",
    line_up_left_double: "╛",
    line_up_right: "└",
    line_up_right_arc: "╰",
    line_up_bold_right_bold: "┗",
    line_up_bold_right: "┖",
    line_up_right_bold: "┕",
    line_up_double_right_double: "╚",
    line_up_double_right: "╙",
    line_up_right_double: "╘",
    line_up_down_left: "┤",
    line_up_bold_down_bold_left_bold: "┫",
    line_up_bold_down_bold_left: "┨",
    line_up_down_left_bold: "┥",
    line_up_bold_down_left_bold: "┩",
    line_up_down_bold_left_bold: "┪",
    line_up_down_bold_left: "┧",
    line_up_bold_down_left: "┦",
    line_up_double_down_double_left_double: "╣",
    line_up_double_down_double_left: "╢",
    line_up_down_left_double: "╡",
    line_up_down_right: "├",
    line_up_bold_down_bold_right_bold: "┣",
    line_up_bold_down_bold_right: "┠",
    line_up_down_right_bold: "┝",
    line_up_bold_down_right_bold: "┡",
    line_up_down_bold_right_bold: "┢",
    line_up_down_bold_right: "┟",
    line_up_bold_down_right: "┞",
    line_up_double_down_double_right_double: "╠",
    line_up_double_down_double_right: "╟",
    line_up_down_right_double: "╞",
    line_down_left_right: "┬",
    line_down_bold_left_bold_right_bold: "┳",
    line_down_left_bold_right_bold: "┯",
    line_down_bold_left_right: "┰",
    line_down_bold_left_bold_right: "┱",
    line_down_bold_left_right_bold: "┲",
    line_down_left_right_bold: "┮",
    line_down_left_bold_right: "┭",
    line_down_double_left_double_right_double: "╦",
    line_down_double_left_right: "╥",
    line_down_left_double_right_double: "╤",
    line_up_left_right: "┴",
    line_up_bold_left_bold_right_bold: "┻",
    line_up_left_bold_right_bold: "┷",
    line_up_bold_left_right: "┸",
    line_up_bold_left_bold_right: "┹",
    line_up_bold_left_right_bold: "┺",
    line_up_left_right_bold: "┶",
    line_up_left_bold_right: "┵",
    line_up_double_left_double_right_double: "╩",
    line_up_double_left_right: "╨",
    line_up_left_double_right_double: "╧",
    line_up_down_left_right: "┼",
    line_up_bold_down_bold_left_bold_right_bold: "╋",
    line_up_down_bold_left_bold_right_bold: "╈",
    line_up_bold_down_left_bold_right_bold: "╇",
    line_up_bold_down_bold_left_right_bold: "╊",
    line_up_bold_down_bold_left_bold_right: "╉",
    line_up_bold_down_left_right: "╀",
    line_up_down_bold_left_right: "╁",
    line_up_down_left_bold_right: "┽",
    line_up_down_left_right_bold: "┾",
    line_up_bold_down_bold_left_right: "╂",
    line_up_down_left_bold_right_bold: "┿",
    line_up_bold_down_left_bold_right: "╃",
    line_up_bold_down_left_right_bold: "╄",
    line_up_down_bold_left_bold_right: "╅",
    line_up_down_bold_left_right_bold: "╆",
    line_up_double_down_double_left_double_right_double: "╬",
    line_up_double_down_double_left_right: "╫",
    line_up_down_left_double_right_double: "╪",
    line_cross: "╳",
    line_backslash: "╲",
    line_slash: "╱",
    tick: "✔",
    info: "ℹ",
    warning: "⚠",
    cross: "✘",
    square_small: "◻",
    square_small_filled: "◼",
    circle: "◯",
    circle_filled: "◉",
    circle_dotted: "◌",
    circle_double: "◎",
    circle_circle: "ⓞ",
    circle_cross: "ⓧ",
    circle_pipe: "Ⓘ",
    radio_on: "◉",
    radio_off: "◯",
    checkbox_on: "☒",
    checkbox_off: "☐",
    checkbox_circle_on: "ⓧ",
    checkbox_circle_off: "Ⓘ",
    pointer: "❯",
    triangle_up_outline: "△",
    triangle_left: "◀",
    triangle_right: "▶",
    lozenge: "◆",
    lozenge_outline: "◇",
    hamburger: "☰",
    smiley: "㋡",
    mustache: "෴",
    star: "★",
    play: "▶",
    nodejs: "⬢",
    one_seventh: "⅐",
    one_ninth: "⅑",
    one_tenth: "⅒",
};

pub const FALLBACK_SYMBOLS: FigureSet = FigureSet {
    circle_question_mark: "(?)",
    question_mark_prefix: "(?)",
    square: "█",
    square_dark_shade: "▓",
    square_medium_shade: "▒",
    square_light_shade: "░",
    square_top: "▀",
    square_bottom: "▄",
    square_left: "▌",
    square_right: "▐",
    square_center: "■",
    bullet: "●",
    dot: "․",
    ellipsis: "…",
    pointer_small: "›",
    triangle_up: "▲",
    triangle_up_small: "▴",
    triangle_down: "▼",
    triangle_down_small: "▾",
    triangle_left_small: "◂",
    triangle_right_small: "▸",
    home: "⌂",
    heart: "♥",
    music_note: "♪",
    music_note_beamed: "♫",
    arrow_up: "↑",
    arrow_down: "↓",
    arrow_left: "←",
    arrow_right: "→",
    arrow_left_right: "↔",
    arrow_up_down: "↕",
    almost_equal: "≈",
    not_equal: "≠",
    less_or_equal: "≤",
    greater_or_equal: "≥",
    identical: "≡",
    infinity: "∞",
    subscript_zero: "₀",
    subscript_one: "₁",
    subscript_two: "₂",
    subscript_three: "₃",
    subscript_four: "₄",
    subscript_five: "₅",
    subscript_six: "₆",
    subscript_seven: "₇",
    subscript_eight: "₈",
    subscript_nine: "₉",
    one_half: "½",
    one_third: "⅓",
    one_quarter: "¼",
    one_fifth: "⅕",
    one_sixth: "⅙",
    one_eighth: "⅛",
    two_thirds: "⅔",
    two_fifths: "⅖",
    three_quarters: "¾",
    three_fifths: "⅗",
    three_eighths: "⅜",
    four_fifths: "⅘",
    five_sixths: "⅚",
    five_eighths: "⅝",
    seven_eighths: "⅞",
    line: "─",
    line_bold: "━",
    line_double: "═",
    line_dashed0: "┄",
    line_dashed1: "┅",
    line_dashed2: "┈",
    line_dashed3: "┉",
    line_dashed4: "╌",
    line_dashed5: "╍",
    line_dashed6: "╴",
    line_dashed7: "╶",
    line_dashed8: "╸",
    line_dashed9: "╺",
    line_dashed10: "╼",
    line_dashed11: "╾",
    line_dashed12: "−",
    line_dashed13: "–",
    line_dashed14: "‐",
    line_dashed15: "⁃",
    line_vertical: "│",
    line_vertical_bold: "┃",
    line_vertical_double: "║",
    line_vertical_dashed0: "┆",
    line_vertical_dashed1: "┇",
    line_vertical_dashed2: "┊",
    line_vertical_dashed3: "┋",
    line_vertical_dashed4: "╎",
    line_vertical_dashed5: "╏",
    line_vertical_dashed6: "╵",
    line_vertical_dashed7: "╷",
    line_vertical_dashed8: "╹",
    line_vertical_dashed9: "╻",
    line_vertical_dashed10: "╽",
    line_vertical_dashed11: "╿",
    line_down_left: "┐",
    line_down_left_arc: "╮",
    line_down_bold_left_bold: "┓",
    line_down_bold_left: "┒",
    line_down_left_bold: "┑",
    line_down_double_left_double: "╗",
    line_down_double_left: "╖",
    line_down_left_double: "╕",
    line_down_right: "┌",
    line_down_right_arc: "╭",
    line_down_bold_right_bold: "┏",
    line_down_bold_right: "┎",
    line_down_right_bold: "┍",
    line_down_double_right_double: "╔",
    line_down_double_right: "╓",
    line_down_right_double: "╒",
    line_up_left: "┘",
    line_up_left_arc: "╯",
    line_up_bold_left_bold: "┛",
    line_up_bold_left: "┚",
    line_up_left_bold: "┙",
    line_up_double_left_double: "╝",
    line_up_double_left: "╜",
    line_up_left_double: "╛",
    line_up_right: "└",
    line_up_right_arc: "╰",
    line_up_bold_right_bold: "┗",
    line_up_bold_right: "┖",
    line_up_right_bold: "┕",
    line_up_double_right_double: "╚",
    line_up_double_right: "╙",
    line_up_right_double: "╘",
    line_up_down_left: "┤",
    line_up_bold_down_bold_left_bold: "┫",
    line_up_bold_down_bold_left: "┨",
    line_up_down_left_bold: "┥",
    line_up_bold_down_left_bold: "┩",
    line_up_down_bold_left_bold: "┪",
    line_up_down_bold_left: "┧",
    line_up_bold_down_left: "┦",
    line_up_double_down_double_left_double: "╣",
    line_up_double_down_double_left: "╢",
    line_up_down_left_double: "╡",
    line_up_down_right: "├",
    line_up_bold_down_bold_right_bold: "┣",
    line_up_bold_down_bold_right: "┠",
    line_up_down_right_bold: "┝",
    line_up_bold_down_right_bold: "┡",
    line_up_down_bold_right_bold: "┢",
    line_up_down_bold_right: "┟",
    line_up_bold_down_right: "┞",
    line_up_double_down_double_right_double: "╠",
    line_up_double_down_double_right: "╟",
    line_up_down_right_double: "╞",
    line_down_left_right: "┬",
    line_down_bold_left_bold_right_bold: "┳",
    line_down_left_bold_right_bold: "┯",
    line_down_bold_left_right: "┰",
    line_down_bold_left_bold_right: "┱",
    line_down_bold_left_right_bold: "┲",
    line_down_left_right_bold: "┮",
    line_down_left_bold_right: "┭",
    line_down_double_left_double_right_double: "╦",
    line_down_double_left_right: "╥",
    line_down_left_double_right_double: "╤",
    line_up_left_right: "┴",
    line_up_bold_left_bold_right_bold: "┻",
    line_up_left_bold_right_bold: "┷",
    line_up_bold_left_right: "┸",
    line_up_bold_left_bold_right: "┹",
    line_up_bold_left_right_bold: "┺",
    line_up_left_right_bold: "┶",
    line_up_left_bold_right: "┵",
    line_up_double_left_double_right_double: "╩",
    line_up_double_left_right: "╨",
    line_up_left_double_right_double: "╧",
    line_up_down_left_right: "┼",
    line_up_bold_down_bold_left_bold_right_bold: "╋",
    line_up_down_bold_left_bold_right_bold: "╈",
    line_up_bold_down_left_bold_right_bold: "╇",
    line_up_bold_down_bold_left_right_bold: "╊",
    line_up_bold_down_bold_left_bold_right: "╉",
    line_up_bold_down_left_right: "╀",
    line_up_down_bold_left_right: "╁",
    line_up_down_left_bold_right: "┽",
    line_up_down_left_right_bold: "┾",
    line_up_bold_down_bold_left_right: "╂",
    line_up_down_left_bold_right_bold: "┿",
    line_up_bold_down_left_bold_right: "╃",
    line_up_bold_down_left_right_bold: "╄",
    line_up_down_bold_left_bold_right: "╅",
    line_up_down_bold_left_right_bold: "╆",
    line_up_double_down_double_left_double_right_double: "╬",
    line_up_double_down_double_left_right: "╫",
    line_up_down_left_double_right_double: "╪",
    line_cross: "╳",
    line_backslash: "╲",
    line_slash: "╱",
    tick: "√",
    info: "i",
    warning: "‼",
    cross: "×",
    square_small: "□",
    square_small_filled: "■",
    circle: "( )",
    circle_filled: "(*)",
    circle_dotted: "( )",
    circle_double: "( )",
    circle_circle: "(○)",
    circle_cross: "(×)",
    circle_pipe: "(│)",
    radio_on: "(*)",
    radio_off: "( )",
    checkbox_on: "[×]",
    checkbox_off: "[ ]",
    checkbox_circle_on: "(×)",
    checkbox_circle_off: "( )",
    pointer: ">",
    triangle_up_outline: "∆",
    triangle_left: "◄",
    triangle_right: "►",
    lozenge: "♦",
    lozenge_outline: "◊",
    hamburger: "≡",
    smiley: "☺",
    mustache: "┌─┐",
    star: "✶",
    play: "►",
    nodejs: "♦",
    one_seventh: "1/7",
    one_ninth: "1/9",
    one_tenth: "1/10",
};

/// Mirrors `is-unicode-supported`, the dependency used by npm `figures`.
pub fn is_unicode_supported() -> bool {
    let term = crate::utils::process_env::var("TERM");
    let term_program = crate::utils::process_env::var("TERM_PROGRAM");
    let wt_session = crate::utils::process_env::var("WT_SESSION");
    let terminus_sublime = crate::utils::process_env::var("TERMINUS_SUBLIME");
    let con_emu_task = crate::utils::process_env::var("ConEmuTask");
    let terminal_emulator = crate::utils::process_env::var("TERMINAL_EMULATOR");

    is_unicode_supported_with_env(
        cfg!(windows),
        term.as_deref(),
        term_program.as_deref(),
        wt_session.as_deref(),
        terminus_sublime.as_deref(),
        con_emu_task.as_deref(),
        terminal_emulator.as_deref(),
    )
}

pub fn is_unicode_supported_with_env(
    is_windows: bool,
    term: Option<&str>,
    term_program: Option<&str>,
    wt_session: Option<&str>,
    terminus_sublime: Option<&str>,
    con_emu_task: Option<&str>,
    terminal_emulator: Option<&str>,
) -> bool {
    if !is_windows {
        return term != Some("linux");
    }

    wt_session.truthy().is_some()
        || terminus_sublime.truthy().is_some()
        || con_emu_task == Some("{cmd::Cmder}")
        || term_program == Some("Terminus-Sublime")
        || term_program == Some("vscode")
        || matches!(
            term,
            Some("xterm-256color" | "alacritty" | "rxvt-unicode" | "rxvt-unicode-256color")
        )
        || terminal_emulator == Some("JetBrains-JediTerm")
}

/// Mirrors npm `figures`' module-level `const shouldUseMain =
/// isUnicodeSupported()`: computed at import, before settings env applies.
/// `entrypoints/cli.rs` forces it in the startup window.
pub(crate) static SHOULD_USE_MAIN: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(is_unicode_supported);

/// Returns the active figures set for the current terminal.
pub fn figures() -> &'static FigureSet {
    if *SHOULD_USE_MAIN {
        &MAIN_SYMBOLS
    } else {
        &FALLBACK_SYMBOLS
    }
}

/// Alias matching the rest of Cometix helper naming.
pub fn get() -> &'static FigureSet {
    figures()
}

/// Mirrors upstream `replaceSymbols(string, {useFallback})`.
pub fn replace_symbols(input: &str, use_fallback: Option<bool>) -> String {
    let use_fallback = use_fallback.unwrap_or(!*SHOULD_USE_MAIN);
    if !use_fallback {
        return input.to_string();
    }

    let mut out = input.to_string();
    for (main, fallback) in SPECIAL_REPLACEMENTS {
        out = out.replace(main, fallback);
    }
    out
}

const SPECIAL_REPLACEMENTS: &[(&str, &str)] = &[
    ("✔", "√"),
    ("ℹ", "i"),
    ("⚠", "‼"),
    ("✘", "×"),
    ("◻", "□"),
    ("◼", "■"),
    ("◯", "( )"),
    ("◉", "(*)"),
    ("◌", "( )"),
    ("◎", "( )"),
    ("ⓞ", "(○)"),
    ("ⓧ", "(×)"),
    ("Ⓘ", "(│)"),
    ("◉", "(*)"),
    ("◯", "( )"),
    ("☒", "[×]"),
    ("☐", "[ ]"),
    ("ⓧ", "(×)"),
    ("Ⓘ", "( )"),
    ("❯", ">"),
    ("△", "∆"),
    ("◀", "◄"),
    ("▶", "►"),
    ("◆", "♦"),
    ("◇", "◊"),
    ("☰", "≡"),
    ("㋡", "☺"),
    ("෴", "┌─┐"),
    ("★", "✶"),
    ("▶", "►"),
    ("⬢", "♦"),
    ("⅐", "1/7"),
    ("⅑", "1/9"),
    ("⅒", "1/10"),
];

// Claude Code local constants from `../rebuild/src/constants/figures.ts`.
#[cfg(target_os = "macos")]
pub const BLACK_CIRCLE: &str = "⏺";
#[cfg(not(target_os = "macos"))]
pub const BLACK_CIRCLE: &str = "●";
pub const BULLET_OPERATOR: &str = "∙";
pub const TEARDROP_ASTERISK: &str = "✻";
pub const UP_ARROW: &str = "↑";
pub const DOWN_ARROW: &str = "↓";
pub const LIGHTNING_BOLT: &str = "↯";
pub const EFFORT_LOW: &str = "○";
pub const EFFORT_MEDIUM: &str = "◐";
pub const EFFORT_HIGH: &str = "●";
pub const EFFORT_XHIGH: &str = "◉";
pub const EFFORT_MAX: &str = "◈";
pub const ULTRACODE_FIGURE: &str = "✦";
pub const PLAY_ICON: &str = "▶";
pub const PAUSE_ICON: &str = "⏸";
pub const REFRESH_ARROW: &str = "↻";
pub const CHANNEL_ARROW: &str = "←";
pub const INJECTED_ARROW: &str = "→";
pub const FORK_GLYPH: &str = "⑂";
pub const DIAMOND_OPEN: &str = "◇";
pub const DIAMOND_FILLED: &str = "◆";
pub const REFERENCE_MARK: &str = "※";
pub const FLAG_ICON: &str = "⚑";
pub const BLOCKQUOTE_BAR: &str = "▎";
pub const HEAVY_HORIZONTAL: &str = "━";
pub const BRIDGE_READY_INDICATOR: &str = "·✔︎·";
pub const BRIDGE_FAILED_INDICATOR: &str = "×";
pub const BRIDGE_SPINNER_FRAMES: &[&str] = &["·|·", "·/·", "·—·", "·\\·"];

// Backward-compatible Cometix aliases. Prefer `figures().pointer`, etc. in new code.
pub const PROMPT_CHAR: &str = ">";
pub const ASSISTANT_MARKER: &str = "◆";
pub const SYSTEM_MARKER: &str = "●";
pub const TOOL_RUNNING: &str = "⟳";
pub const TOOL_DONE: &str = "✓";
pub const TOOL_FAILED: &str = "✗";
pub const THINKING: &str = "…";
pub const COLLAPSED: &str = "▸";
pub const EXPANDED: &str = "▾";
pub const HORIZONTAL_LINE: &str = "─";
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_and_fallback_symbols_match_upstream_examples() {
        assert_eq!(MAIN_SYMBOLS.pointer, "❯");
        assert_eq!(FALLBACK_SYMBOLS.pointer, ">");
        assert_eq!(MAIN_SYMBOLS.line_down_right, "┌");
        assert_eq!(MAIN_SYMBOLS.line_up_down_left_right, "┼");
    }

    #[test]
    fn unicode_support_matches_is_unicode_supported_rules() {
        assert!(!is_unicode_supported_with_env(
            false,
            Some("linux"),
            None,
            None,
            None,
            None,
            None
        ));
        assert!(is_unicode_supported_with_env(
            false,
            Some("xterm-256color"),
            None,
            None,
            None,
            None,
            None
        ));
        assert!(is_unicode_supported_with_env(
            true,
            Some("xterm-256color"),
            None,
            None,
            None,
            None,
            None
        ));
        assert!(!is_unicode_supported_with_env(
            true,
            Some("dumb"),
            None,
            Some(""),
            Some(""),
            None,
            None
        ));
        assert!(is_unicode_supported_with_env(
            true,
            Some("dumb"),
            None,
            Some("1"),
            None,
            None,
            None
        ));
    }

    #[test]
    fn replace_symbols_mirrors_upstream_special_replacements() {
        assert_eq!(replace_symbols("❯ ✔ ⚠", Some(true)), "> √ ‼");
        assert_eq!(replace_symbols("❯ ✔ ⚠", Some(false)), "❯ ✔ ⚠");
    }

    #[test]
    fn claude_local_figures_are_available() {
        assert_eq!(BLOCKQUOTE_BAR, "▎");
        assert_eq!(HEAVY_HORIZONTAL, "━");
        assert_eq!(TEARDROP_ASTERISK, "✻");
    }
}
