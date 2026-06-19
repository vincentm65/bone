use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

struct SpinnerDef {
    name: &'static str,
    frames: Vec<&'static str>,
    speed: u64,
}

fn char_width(c: char) -> usize {
    let cp = c as u32;
    if (0x1100..=0x115F).contains(&cp)
        || ((0x2E80..=0xA4CF).contains(&cp) && cp != 0x303F)
        || (0xAC00..=0xD7A3).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE10..=0xFE19).contains(&cp)
        || (0xFE30..=0xFE6F).contains(&cp)
        || (0xFF00..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F000..=0x1FFFF).contains(&cp)
    {
        2
    } else {
        1
    }
}

fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn pad_cell(s: &str, width: usize) -> String {
    let dw = display_width(s);
    if dw >= width {
        let mut out = String::new();
        let mut w = 0;
        for c in s.chars() {
            let cw = char_width(c);
            if w + cw > width { break; }
            out.push(c);
            w += cw;
        }
        out
    } else {
        format!("{}{}", s, " ".repeat(width - dw))
    }
}

fn main() {
    let spinners: Vec<SpinnerDef> = vec![
        // === ORIGINALS ===
        SpinnerDef { name: "braille", frames: vec!["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"], speed: 80 },
        SpinnerDef { name: "block", frames: vec!["⣀","⣄","⣤","⣦","⣶","⣷","⣿","⣾","⣽","⣻","⢿","⡿","⣟","⡽","⣾","⣽","⣷","⣶","⣦","⣤","⣀"], speed: 60 },
        SpinnerDef { name: "dots", frames: vec!["◜","◠","◝","◞","◡","◟"], speed: 100 },
        SpinnerDef { name: "slash", frames: vec!["-","\\","|","/"], speed: 80 },
        SpinnerDef { name: "plus", frames: vec!["✚","✹","✸","✷","✶","✵","✴","✳","✲","✱","✰"], speed: 60 },
        SpinnerDef { name: "wavebar", frames: vec!["▁","▂","▃","▄","▅","▆","▇","▆","▅","▄","▃","▂"], speed: 80 },
        SpinnerDef { name: "bracket", frames: vec!["[    ]","[   -]","[  --]","[ ---]","[----]","[ ---]","[  --]","[   -]","[    ]","[   /]","[  //]","[ ///]","[////]","[ ///]","[  //]","[   /]"], speed: 70 },
        SpinnerDef { name: "triangle", frames: vec!["▲","▶","▼","◀"], speed: 120 },
        SpinnerDef { name: "pipe", frames: vec!["▏","▎","▍","▌","▋","▊","▉","▊","▋","▌","▍","▎"], speed: 60 },

        // === NEW ===
        // Loading bar with percentage
        SpinnerDef { name: "loadbar", frames: vec![
            "[░░░░░░░░]  0%", "[░░░░░░░▒] 10%", "[░░░░░░▒▒] 20%", "[░░░░░▒▒▒] 30%",
            "[░░░░▒▒▒▒] 40%", "[░░░▒▒▒▒▒] 50%", "[░░▒▒▒▒▒▒] 60%", "[░▒▒▒▒▒▒▒] 70%",
            "[▒▒▒▒▒▒▒▒] 80%", "[▒▒▒▒▒▒▒█] 90%", "[████████] 95%", "[████████] 100%",
        ], speed: 100 },

        // Terminal prompt
        SpinnerDef { name: "terminal", frames: vec![
            "$ _", "$  _", "$   _", "$    _", "$     _", "$      _",
            "$       _", "$        _", "$         _", "$          _",
            "$           _", "$            _", "$             _", "$              _",
            "$               _", "$                _", "$                 _", "$                  _",
            "$                   _", "$                    _", "$                     _", "$                      _",
            "$                       _", "$                        _", "$                         _", "$                          _",
            "$                           _", "$                            _", "$                             _", "$                              _",
            "$                               _", "$                                _", "$                                 _", "$                                  _",
            "$                                   _", "$                                    _", "$                                     _", "$                                      _",
            "$                                       _", "$                                        _", "$                                         _", "$                                          _",
            "$                                           _", "$                                            _", "$                                             _", "$                                              _",
            "$                                               _", "$                                                _", "$                                                 _", "$                                                  _",
            "$                                                   _", "$                                                    _", "$                                                     _", "$                                                      _",
            "# _", "#  _", "#   _", "#    _", "#     _", "#      _",
            "#       _", "#        _", "#         _", "#          _",
            "#           _", "#            _", "#             _", "#              _",
            "#               _", "#                _", "#                 _", "#                  _",
            "#                   _", "#                    _", "#                     _", "#                      _",
            "#                       _", "#                        _", "#                         _", "#                          _",
            "#                           _", "#                            _", "#                             _", "#                              _",
            "#                               _", "#                                _", "#                                 _", "#                                  _",
            "#                                   _", "#                                    _", "#                                     _", "#                                      _",
            "#                                       _", "#                                        _", "#                                         _", "#                                          _",
            "#                                           _", "#                                            _", "#                                             _", "#                                              _",
            "#                                               _", "#                                                _", "#                                                 _", "#                                                  _",
        ], speed: 50 },

        // Countdown text
        SpinnerDef { name: "countdown", frames: vec![
            "loading", "loadin", "loadi", "load", "loa", "lo", "l",
            "l", "lo", "loa", "load", "loadi", "loadin", "loading",
            "loadin", "loadi", "load", "loa", "lo", "l",
            "l", "lo", "loa", "load", "loadi", "loadin", "loading",
        ], speed: 120 },

        // Kaomoji wave
        SpinnerDef { name: "kaomoji", frames: vec![
            "〜(￣▽￣)〜", "〜(▰￣▽￣▰)〜", "〜(◕‿◕)〜", "〜(◕ᴗ◕✿)〜",
            "〜(✿◠‿◠)〜", "〜(◕ᴗ◕✿)〜", "〜(◕‿◕)〜", "〜(▰￣▽￣▰)〜",
        ], speed: 150 },

        // Pulsing circle
        SpinnerDef { name: "pulse", frames: vec![
            "●", "◉", "◎", "●", "◉", "◎",
            "◎", "◉", "●", "◎", "◉", "●",
        ], speed: 80 },

        // Radar sweep (shorter)
        SpinnerDef { name: "radar", frames: vec![
            "◔○", "◕○", "○◔", "○◕",
            "◖○", "◗○", "○◖", "○◗",
            "◘○", "◙○", "○◘", "○◙",
        ], speed: 80 },

        // Progress ring
        SpinnerDef { name: "prog_ring", frames: vec![
            "(░░░░░░░░)", "(▒░░░░░░░)", "(▒▒░░░░░░)", "(▒▒▒░░░░░)", "(▒▒▒▒░░░░)", "(▒▒▒▒▒░░░)",
            "(▒▒▒▒▒▒░░)", "(▒▒▒▒▒▒▒░)", "(▒▒▒▒▒▒▒▒)", "(█▒▒▒▒▒▒▒)", "(██▒▒▒▒▒▒)", "(███▒▒▒▒▒)",
            "(████▒▒▒▒)", "(█████▒▒▒)", "(██████▒▒)", "(███████▒)", "(████████)",
        ], speed: 100 },

        // Typing dots
        SpinnerDef { name: "typing", frames: vec![
            ".", ". .", ".. .", "...", "... ", ".. .", ". .", ".",
            ". .", ".. .", "...", "... ", ".. .", ". .", ".",
            ". .", ".. .", "...",
        ], speed: 150 },

        // Matrix rain
        SpinnerDef { name: "matrix", frames: vec![
            "┌┬┐", "│││", "└┴┘", "┌┬┐", "│││", "└┴┘",
        ], speed: 200 },

        // Heartbeat
        SpinnerDef { name: "heartbeat", frames: vec![
            "♫♫♫", "♫♫♪", "♫♪♪", "♪♪♪", "♫♪♪", "♫♫♪",
        ], speed: 120 },

        // Gauge
        SpinnerDef { name: "gauge", frames: vec![
            "[-----]", "[----+]", "[---++]", "[--++]", "[-++]", "[+++++]",
            "[+++++]", "[+++-]", "[++--]", "[+---]", "[----]", "[-----]",
        ], speed: 100 },

        // Wave line
        SpinnerDef { name: "waveline", frames: vec![
            "▁▃▅▇▅▃▁", "▃▅▇▅▃▁▂", "▅▇▅▃▁▂▄", "▇▅▃▁▂▄▆",
            "▅▃▁▂▄▆▇", "▃▁▂▄▆▇▅", "▁▂▄▆▇▅▃", "▂▄▆▇▅▃▁",
        ], speed: 100 },

        // Loading text with dots
        SpinnerDef { name: "dots_text", frames: vec![
            "loading", "loading.", "loading..", "loading...", "loading...", "loading..",
            "loading.", "loading", "loading.", "loading..", "loading...", "loading...",
            "loading..", "loading.", "loading",
        ], speed: 150 },

        // Spiral
        SpinnerDef { name: "spiral", frames: vec![
            "◤◢◥◣", "◢◥◣◤", "◥◣◤◢", "◣◤◢◥",
        ], speed: 100 },

        // Progress text block
        SpinnerDef { name: "progblock", frames: vec![
            "░░░░░░░░", "▒░░░░░░░", "▒▒░░░░░░", "▒▒▒░░░░░",
            "▒▒▒▒░░░░", "▒▒▒▒▒░░░", "▒▒▒▒▒▒░░", "▒▒▒▒▒▒▒░",
            "▒▒▒▒▒▒▒▒", "█▒▒▒▒▒▒▒", "██▒▒▒▒▒▒", "███▒▒▒▒▒",
            "████▒▒▒▒", "█████▒▒▒", "██████▒▒", "███████▒",
            "████████",
        ], speed: 100 },
    ];

    let cols = 4;
    let rows = (spinners.len() as f64 + (cols - 1) as f64) / cols as f64;
    let rows = rows as usize;

    let stdout = io::stdout();
    let mut out = stdout.lock();

    print!("\x1b[2J\x1b[H");
    writeln!(out, "  Spinner Styles Demo").unwrap();
    writeln!(out, "  Press Ctrl+C to exit\n").unwrap();

    let start = Instant::now();
    let mut ticks: Vec<usize> = vec![0; spinners.len()];
    let mut last_times: Vec<Instant> = vec![Instant::now(); spinners.len()];

    loop {
        let now = Instant::now();
        let mut need_redraw = false;

        for i in 0..spinners.len() {
            let elapsed = now.duration_since(last_times[i]).as_millis();
            if elapsed >= spinners[i].speed as u128 {
                ticks[i] += 1;
                last_times[i] = now;
                need_redraw = true;
            }
        }

        if need_redraw {
            print!("\x1b[H");

            for row in 0..rows {
                let mut line = String::new();
                for col in 0..cols {
                    let idx = row + col * rows;
                    if idx < spinners.len() {
                        let frame = spinners[idx].frames[ticks[idx] % spinners[idx].frames.len()];
                        let name = pad_cell(spinners[idx].name, 12);
                        let frame = pad_cell(frame, 16);
                        line.push_str(&format!("  {} {}  ", name, frame));
                    } else {
                        line.push_str("             ");
                    }
                }
                println!("{}", line);
            }

            let secs = start.elapsed().as_secs();
            println!("\n  Running for {:02}:{:02}  |  {} spinners active", secs / 60, secs % 60, spinners.len());
        }

        thread::sleep(Duration::from_millis(10));
    }
}
