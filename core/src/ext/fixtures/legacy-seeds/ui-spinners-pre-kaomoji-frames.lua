-- Spinner + thinking-text presets consumed by the Rust renderer.
-- Required by the boot snapshot (ui.spinners). Edit here to add/adjust styles.
--
-- Each spinner: { name = ..., speed = <ms/frame>, frames = { ... } }
-- Each text:    { name = ..., phrases = { ... } }  (rotates one phrase per cycle)
return {
  spinners = {
    { name = "braille",   speed = 80,  frames = { "⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏" } },
    { name = "triangle",  speed = 120, frames = { "▲","▶","▼","◀" } },
    { name = "pipe",      speed = 60,  frames = { "▏","▎","▍","▌","▋","▊","▉","▊","▋","▌","▍","▎" } },
    { name = "kaomoji",   speed = 150, frames = { "〜(￣▽￣)〜","〜(▰￣▽￣▰)〜","〜(◕‿◕)〜","〜(◕ᴗ◕✿)〜","〜(✿◠‿◠)〜","〜(◕ᴗ◕✿)〜","〜(◕‿◕)〜","〜(▰￣▽￣▰)〜" } },
    { name = "typing",    speed = 150, frames = { ".",". .",".. .","...","... ",".. .",". .","." } },
    { name = "waveline",  speed = 100, frames = { "▁▃▅▇▅▃▁","▃▅▇▅▃▁▂","▅▇▅▃▁▂▄","▇▅▃▁▂▄▆","▅▃▁▂▄▆▇","▃▁▂▄▆▇▅","▁▂▄▆▇▅▃","▂▄▆▇▅▃▁" } },
    { name = "dots_text", speed = 150, frames = { "loading","loading.","loading..","loading..." } },
    { name = "progblock", speed = 100, frames = { "░░░░░░░░","▒░░░░░░░","▒▒░░░░░░","▒▒▒░░░░░","▒▒▒▒░░░░","▒▒▒▒▒░░░","▒▒▒▒▒▒░░","▒▒▒▒▒▒▒░","▒▒▒▒▒▒▒▒","█▒▒▒▒▒▒▒","██▒▒▒▒▒▒","███▒▒▒▒▒","████▒▒▒░","█████▒▒░","██████▒░","███████░","████████" } },
  },
  texts = {
    { name = "thinking",   phrases = { "thinking" } },
    { name = "pondering",  phrases = { "thinking","reasoning","pondering" } },
    { name = "processing", phrases = { "processing","computing","crunching" } },
  },
}
