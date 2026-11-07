# Files

A file explorer, a file viewer and a text editor, in the desktop app
(`bone --gui`). One plugin, three things to do with the files on disk.

## What it does

`/files` opens the explorer: the folders of the working directory on the
left, and what the selected file holds on the right. Each entry shows its
size and modification date; drag the explorer's right edge to change its
width. The editor window can be resized the same way.

| File | What the preview shows |
|---|---|
| Markdown (`.md`, `.markdown`, …) | the document, rendered |
| HTML (`.html`, `.htm`) | the page's text, without the markup |
| PDF (`.pdf`) | its text, read by the plugin itself |
| Pictures (`.png`, `.jpg`, `.webp`, …) | the picture |
| Anything else | its text, as it is |

`e` opens the selected file in the editor, a window of its own over the
explorer: the file's lines with a cursor in them, and a Markdown preview
beside them (`ctrl+p`). `ctrl+s` saves.

`o` opens the selected file with the desktop's own app (the system's
default one for it).

## The keys

**The explorer**

| Key | What it does |
|---|---|
| `↑` `↓` | move the selection |
| `enter` | open a folder, or show a file |
| `e` | edit the selected file |
| `v` | show the selected file |
| `o` | open it with the desktop's app |
| `backspace` | one folder up |
| `r` | read the folder again |
| `ctrl+h` | show the hidden files (and hide them again) |
| `esc` | close |

**The editor**

| Key | What it does |
|---|---|
| `ctrl+s` | save |
| `ctrl+z` | undo |
| `ctrl+y` (or `ctrl+shift+z`) | redo |
| `ctrl+p` | the Markdown preview, beside the text |
| `enter` `backspace` `delete` `tab` | edit |
| `↑` `↓` `←` `→` `home` `end` | move the cursor |
| `esc` | close (twice, if there are unsaved changes) |

Typing a word and taking it back is one step: a run of the same edit is
one step in the undo stack.

## How the save works

The editor writes the file itself, with `bone.fs.write`: the buffer goes
to a temporary file in the same folder and is renamed over the file, so a
save is never half-written. The editor keeps the text it read, and says
"no changes to save" when the file is already what the buffer holds.

The core half of the plugin (`core.lua`) registers the two things the
desktop cannot do by itself, for the desktop half to call:

- `files.pdftext` — a PDF's text, read by the plugin (`files.pdf`): the
  file's objects, its streams and the text they hold
- `files.open` — open a file with the desktop's app (`xdg-open`)

The PDF is read in the plugin, so no other program is needed for it.
`files.open` runs through `bone.system`, so the core waits for it without
blocking anything else.

## The PDF reader

`files.pdf` reads a PDF as it is, in Lua: the objects (a page tree, the
fonts), the streams (inflated in the plugin, with a DEFLATE decoder of
its own) and the text operators in them. It gives the text of the first
twenty pages, with the pages' own fonts (a simple font's codes, or a
font's `ToUnicode` map).

What it cannot read: a scanned page (a picture of a page, with no text in
it) — the preview says so — and a font whose codes are mapped nowhere.

## What it needs

- `xdg-open` for opening a file with the desktop's app.

Nothing else: the PDF is read by the plugin itself.

## The commands

| Command | What it does |
|---|---|
| `/files [path]` | open the explorer (at `path`, or in the working directory) |
| `/explorer` | the same |
| `/editor <path>` | open the editor on a file |
| `/edit` | the same |

The plugin works in the desktop app only: in the terminal, `/files` and
`/editor` say where the windows are.
