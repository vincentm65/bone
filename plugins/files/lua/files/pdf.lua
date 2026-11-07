-- A PDF's text, read here: the file's own objects, its streams and the
-- text they hold. No poppler and no other tool: the PDF is read as it is,
-- in the plugin.
--
-- What it reads: the pages' text, with the pages' own fonts (a simple
-- font's codes, or a font's ToUnicode map). What it cannot: a scanned
-- page (an image, with no text in it) and a font whose codes are mapped
-- nowhere (the page gives no text).

local M = {}

-- `unpack` moved into the table in Lua 5.2.
local unpack = table.unpack or unpack

--- The biggest file it looks at.
M.MAX_BYTES = 16 * 1024 * 1024
--- The most pages it reads.
M.MAX_PAGES = 20
--- The most text it keeps.
M.MAX_TEXT = 128 * 1024
--- The biggest stream it inflates.
M.MAX_STREAM = 8 * 1024 * 1024

-- ---- the DEFLATE decoder (RFC 1951) -------------------------------------

local LENGTH_BASE = { 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43,
  51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258 }
local LENGTH_EXTRA = { 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4,
  5, 5, 5, 5, 0 }
local DIST_BASE = { 1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385,
  513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577 }
local DIST_EXTRA = { 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10,
  11, 11, 12, 12, 13, 13 }

--- A Huffman table: how many codes of each length, and the symbols.
--- `symbols` gives the symbol of each length (the order of the alphabet);
--- without it the lengths are in the alphabet's own order.
local function huffman(lengths, symbols)
  local count = {}
  for i = 0, 15 do count[i] = 0 end
  local entries = {}
  for i, len in ipairs(lengths) do
    if len > 0 then
      count[len] = count[len] + 1
      entries[#entries + 1] = { symbol = symbols and symbols[i] or (i - 1), len = len }
    end
  end
  local left = 1
  for len = 1, 15 do
    left = left * 2 - (count[len] or 0)
    if left < 0 then return nil end
  end
  -- the codes: the shorter ones first, and within a length the smaller
  -- symbols first
  table.sort(entries, function(a, b)
    if a.len ~= b.len then return a.len < b.len end
    return a.symbol < b.symbol
  end)
  local symbol = {}
  for i, e in ipairs(entries) do symbol[i] = e.symbol end
  return { count = count, symbol = symbol }
end

local FIXED_LIT = (function()
  local lengths = {}
  for i = 1, 144 do lengths[i] = 8 end
  for i = 145, 256 do lengths[i] = 9 end
  for i = 257, 280 do lengths[i] = 7 end
  for i = 281, 288 do lengths[i] = 8 end
  return huffman(lengths)
end)()

local FIXED_DIST = (function()
  local lengths = {}
  for i = 1, 32 do lengths[i] = 5 end
  return huffman(lengths)
end)()

--- The inflated bytes of the DEFLATE stream in `data`, from `pos` on.
local function inflate_raw(data, pos)
  local acc, nbits = 0, 0
  local out = {}
  local count = 0

  local function need(n)
    while nbits < n do
      local b = data:byte(pos)
      if not b then return false end
      pos = pos + 1
      acc = acc + b * 2 ^ nbits
      nbits = nbits + 8
    end
    return true
  end

  local function take(n)
    if not need(n) then return nil end
    local v = acc % 2 ^ n
    acc = math.floor(acc / 2 ^ n)
    nbits = nbits - n
    return v
  end

  local function decode(huff)
    local code, first, index = 0, 0, 0
    for len = 1, 15 do
      if nbits == 0 then
        local b = data:byte(pos)
        if not b then return nil end
        pos = pos + 1
        acc, nbits = b, 8
      end
      local bit = acc % 2
      acc = math.floor(acc / 2)
      nbits = nbits - 1
      code = code + bit
      local n = huff.count[len] or 0
      if code - first < n then return huff.symbol[index + (code - first) + 1] end
      index = index + n
      first = (first + n) * 2
      code = code * 2
    end
    return nil
  end

  local function emit(byte)
    count = count + 1
    out[count] = byte
  end

  local last = 0
  repeat
    last = take(1)
    if last == nil then return nil end
    local btype = take(2)
    if btype == nil then return nil end
    if btype == 0 then
      -- a stored block: to the byte, then its length
      local drop = nbits % 8
      acc = math.floor(acc / 2 ^ drop)
      nbits = nbits - drop
      local len = (data:byte(pos) or 0) + (data:byte(pos + 1) or 0) * 256
      local nlen = (data:byte(pos + 2) or 0) + (data:byte(pos + 3) or 0) * 256
      pos = pos + 4
      if (len + nlen) % 65536 ~= 65535 then return nil end
      for _ = 1, len do
        local b = data:byte(pos)
        if not b then return nil end
        pos = pos + 1
        emit(b)
      end
    elseif btype == 1 or btype == 2 then
      local lit, dist
      if btype == 1 then
        lit, dist = FIXED_LIT, FIXED_DIST
      else
        local hlit = take(5)
        local hdist = take(5)
        local hclen = take(4)
        if not hlit then return nil end
        hlit, hdist, hclen = hlit + 257, hdist + 1, hclen + 4
        local order = { 16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15 }
        local clens = {}
        for i = 1, hclen do clens[i] = take(3) or 0 end
        local chuff = huffman(clens, order)
        if not chuff then return nil end
        local lengths = {}
        local n = 0
        while n < hlit + hdist do
          local sym = decode(chuff)
          if sym == nil then return nil end
          if sym < 16 then
            n = n + 1
            lengths[n] = sym
          elseif sym == 16 then
            local prev = lengths[n] or 0
            local rep = take(2) or 0
            for _ = 1, rep + 3 do
              n = n + 1
              lengths[n] = prev
            end
          elseif sym == 17 then
            local rep = take(3) or 0
            for _ = 1, rep + 3 do
              n = n + 1
              lengths[n] = 0
            end
          else
            local rep = take(7) or 0
            for _ = 1, rep + 11 do
              n = n + 1
              lengths[n] = 0
            end
          end
        end
        local litlens, distlens = {}, {}
        for i = 1, hlit do litlens[i] = lengths[i] end
        for i = 1, hdist do distlens[i] = lengths[hlit + i] end
        lit = huffman(litlens)
        dist = huffman(distlens)
        if not lit or not dist then return nil end
      end
      while true do
        local sym = decode(lit)
        if sym == nil then return nil end
        if sym < 256 then
          emit(sym)
        elseif sym == 256 then
          break
        else
          local i = sym - 256
          if i < 1 or i > #LENGTH_BASE then return nil end
          local len = LENGTH_BASE[i] + (take(LENGTH_EXTRA[i]) or 0)
          local dsym = decode(dist)
          if dsym == nil or dsym >= #DIST_BASE then return nil end
          local d = DIST_BASE[dsym + 1] + (take(DIST_EXTRA[dsym + 1]) or 0)
          if d > count then return nil end
          for _ = 1, len do emit(out[count - d + 1]) end
        end
        if count > M.MAX_STREAM then return nil end
      end
    else
      return nil
    end
  until last == 1

  local parts = {}
  local i = 1
  while i <= count do
    local j = math.min(i + 4095, count)
    local chunk = {}
    for k = i, j do chunk[k - i + 1] = out[k] end
    parts[#parts + 1] = string.char(unpack(chunk, 1, j - i + 1))
    i = j + 1
  end
  return table.concat(parts)
end

--- The inflated bytes of a zlib stream (or of a raw DEFLATE one).
local function inflate(data)
  if type(data) ~= "string" or #data < 2 then return nil end
  local cmf = data:byte(1)
  local flg = data:byte(2)
  -- The zlib header: the method (8), the check bits, and no dictionary.
  local fdict = flg and flg >= 32 and (flg - 32) % 64 < 32
  if cmf and flg and cmf % 16 == 8 and (cmf * 256 + flg) % 31 == 0 and not fdict then
    local out = inflate_raw(data, 3)
    if out then return out end
  end
  return inflate_raw(data, 1)
end

-- ---- the file's objects --------------------------------------------------

local function is_space(c)
  return c == " " or c == "\t" or c == "\n" or c == "\r" or c == "\f" or c == "\0"
end

--- The next place to read from, past the space and the comments.
local function skip(data, pos)
  while pos <= #data do
    local c = data:sub(pos, pos)
    if c == "%" then
      local e = data:find("\n", pos, true)
      pos = e and (e + 1) or (#data + 1)
    elseif is_space(c) then
      pos = pos + 1
    else
      return pos
    end
  end
  return pos
end

local name_at, literal_at, hex_at, dict_at, array_at, value_at
local read_object_streams, decode_stream

--- A name: `/Name`, with its `#xx` escapes read.
-- The pattern holds no NUL byte: LuaJIT's matcher cannot read one.
local function read_name(data, pos)
  local s, e = data:find("^/[^\t\n\f\r ()<>[%]{}/%%]*", pos)
  if not s then return nil, pos + 1 end
  local value = data:sub(s + 1, e):gsub("%z", "")
  value = value:gsub("#(%x%x)", function(h) return string.char(tonumber(h, 16)) end)
  return { kind = "name", value = value }, e + 1
end

--- A literal string: `(text)`, with its escapes read.
local function read_literal(data, pos)
  local out = {}
  local depth = 1
  local i = pos
  while i <= #data do
    local c = data:sub(i, i)
    if c == "\\" then
      local n = data:sub(i + 1, i + 1)
      if n == "n" then out[#out + 1] = "\n" i = i + 2
      elseif n == "r" then out[#out + 1] = "\r" i = i + 2
      elseif n == "t" then out[#out + 1] = "\t" i = i + 2
      elseif n == "b" then out[#out + 1] = "\b" i = i + 2
      elseif n == "f" then out[#out + 1] = "\f" i = i + 2
      elseif n == "(" or n == ")" or n == "\\" then out[#out + 1] = n i = i + 2
      elseif n == "\r" then i = i + 2
      elseif n == "\n" then i = i + 2
      else
        local oct = data:match("^%d%d?%d?", i + 1)
        local code = oct and tonumber(oct, 8)
        if code then
          out[#out + 1] = string.char(code % 256)
          i = i + 1 + #oct
        else
          out[#out + 1] = n
          i = i + 2
        end
      end
    elseif c == "(" then
      depth = depth + 1
      out[#out + 1] = c
      i = i + 1
    elseif c == ")" then
      depth = depth - 1
      i = i + 1
      if depth == 0 then break end
      out[#out + 1] = c
    else
      out[#out + 1] = c
      i = i + 1
    end
  end
  return { kind = "string", bytes = table.concat(out) }, i
end

--- A hex string: `<4142>`.
local function read_hex(data, pos)
  local e = data:find(">", pos, true)
  if not e then return nil, pos + 1 end
  local hex = data:sub(pos, e - 1):gsub("%s", "")
  if #hex % 2 == 1 then hex = hex .. "0" end
  local bytes = {}
  for i = 1, #hex, 2 do
    bytes[#bytes + 1] = string.char(tonumber(hex:sub(i, i + 1), 16) or 0)
  end
  return { kind = "string", bytes = table.concat(bytes) }, e + 1
end

--- A dictionary: `<< /Key value >>`.
local function read_dict(data, pos, doc)
  local map = {}
  pos = skip(data, pos + 2)
  while pos <= #data do
    pos = skip(data, pos)
    if data:sub(pos, pos + 1) == ">>" then
      return { kind = "dict", map = map }, pos + 2
    end
    if data:sub(pos, pos) ~= "/" then
      pos = pos + 1
    else
      local key, next_pos = read_name(data, pos)
      pos = next_pos
      local value
      value, pos = value_at(data, pos, doc)
      if key then map[key.value] = value end
    end
  end
  return { kind = "dict", map = map }, pos
end

--- An array: `[a b c]`.
local function read_array(data, pos, doc)
  local items = {}
  pos = skip(data, pos + 1)
  while pos <= #data do
    pos = skip(data, pos)
    if data:sub(pos, pos) == "]" then
      return { kind = "array", items = items }, pos + 1
    end
    local before = pos
    local value
    value, pos = value_at(data, pos, doc)
    if value then items[#items + 1] = value end
    if pos <= before then pos = before + 1 end
  end
  return { kind = "array", items = items }, pos
end

--- One value: a number, a name, a string, a dictionary, an array or a
--- reference to another object.
local function read_value(data, pos, doc)
  pos = skip(data, pos)
  local c = data:sub(pos, pos)
  if c == "" then return nil, pos end
  if c == "<" then
    if data:sub(pos + 1, pos + 1) == "<" then return read_dict(data, pos, doc) end
    return read_hex(data, pos + 1)
  elseif c == "(" then
    return read_literal(data, pos + 1)
  elseif c == "/" then
    return read_name(data, pos)
  elseif c == "[" then
    return read_array(data, pos, doc)
  elseif c == "]" then
    return nil, pos + 1
  end
  local s, e, a, b = data:find("^([+-]?%d+)%s+([+-]?%d+)%s+R", pos)
  if s then
    return { kind = "ref", num = tonumber(a), gen = tonumber(b) }, e + 1
  end
  local num = data:match("^[+-]?%d*%.?%d*", pos)
  if num and num:find("%d") then
    return { kind = "number", value = tonumber(num) }, pos + #num
  end
  local word = data:match("^[A-Za-z'\"][A-Za-z0-9*'\"%%]*", pos)
  if word then
    return { kind = "keyword", value = word }, pos + #word
  end
  return nil, pos + 1
end

name_at, literal_at, hex_at, dict_at, array_at, value_at =
  read_name, read_literal, read_hex, read_dict, read_array, read_value

--- The value a reference stands for.
local function resolve(doc, value, depth)
  depth = depth or 0
  if type(value) ~= "table" then return value end
  if value.kind == "ref" then
    if depth > 32 then return nil end
    local obj = doc.objects[value.num]
    if not obj then return nil end
    return resolve(doc, obj.value, depth + 1)
  end
  return value
end

--- `key` of `dict`, with its references read.
local function get(doc, dict, key)
  if type(dict) ~= "table" or dict.kind ~= "dict" then return nil end
  local value = dict.map[key]
  if value == nil then return nil end
  return resolve(doc, value)
end

--- Every object in the file: its number and its value.
local function scan(data)
  local doc = { objects = {} }
  local pos = 1
  while true do
    local s, e, num = data:find("([0-9]+)%s+([0-9]+)%s+obj", pos)
    if not s then break end
    local value, next_pos = read_value(data, e + 1, doc)
    local obj = { num = tonumber(num), value = value }
    local p = skip(data, next_pos)
    if data:sub(p, p + 5) == "stream" then
      local body = p + 6
      if data:sub(body, body) == "\r" then body = body + 1 end
      if data:sub(body, body) == "\n" then body = body + 1 end
      local es = data:find("endstream", body, true)
      obj.stream = { start = body, stop = es and (es - 1) or #data }
      next_pos = es and (es + 9) or (#data + 1)
    end
    doc.objects[obj.num] = obj
    pos = next_pos
  end
  -- the streams' bytes, with their own length
  for _, obj in pairs(doc.objects) do
    if obj.stream then
      local len = get(doc, obj.value, "Length")
      local start = obj.stream.start
      local stop = obj.stream.stop
      if len and len.kind == "number" and len.value >= 0 and start + len.value <= #data then
        stop = start + len.value
      else
        -- the length is missing or wrong: what is before "endstream"
        while stop >= start and (data:sub(stop, stop) == "\n" or data:sub(stop, stop) == "\r") do
          stop = stop - 1
        end
      end
      obj.stream.data = data:sub(start, stop)
      -- the stream's bytes belong to the object itself
      if type(obj.value) == "table" then obj.value.stream = obj.stream end
    end
  end
  -- the objects that live inside an object stream
  read_object_streams(doc)
  return doc
end

--- The objects an object stream holds: the pairs of numbers in its head
--- say where each of them stands.
read_object_streams = function(doc)
  for _, obj in pairs(doc.objects) do
    local t = get(doc, obj.value, "Type")
    if t and t.kind == "name" and t.value == "ObjStm" then
      local count = get(doc, obj.value, "N")
      local first = get(doc, obj.value, "First")
      local body = decode_stream(doc, obj.value)
      if body and count and first and count.kind == "number" and first.kind == "number" then
        local nums = {}
        for num in body:sub(1, first.value):gmatch("%d+") do nums[#nums + 1] = tonumber(num) end
        local n = count.value
        for i = 1, math.min(n, math.floor(#nums / 2)) do
          local num = nums[2 * i - 1]
          local offset = nums[2 * i]
          local start = first.value + 1 + offset
          if start >= 1 and start <= #body and not doc.objects[num] then
            local value = read_value(body, start, doc)
            if value then doc.objects[num] = { num = num, value = value } end
          end
        end
      end
    end
  end
end

-- ---- the streams' filters ------------------------------------------------
local function hex_decode(data)
  local hex = data:gsub("%s", "")
  hex = hex:gsub(">[^>]*$", "")
  if #hex % 2 == 1 then hex = hex .. "0" end
  local bytes = {}
  for i = 1, #hex, 2 do
    bytes[#bytes + 1] = string.char(tonumber(hex:sub(i, i + 1), 16) or 0)
  end
  return table.concat(bytes)
end

local function ascii85_decode(data)
  local out = {}
  local group = {}
  for i = 1, #data do
    local c = data:sub(i, i)
    if c == "~" then break end
    if is_space(c) then
      -- the space between the groups
    elseif c == "z" and #group == 0 then
      out[#out + 1] = string.char(0, 0, 0, 0)
    else
      group[#group + 1] = string.byte(c)
      if #group == 5 then
        local value = 0
        for _, b in ipairs(group) do value = value * 85 + (b - 33) end
        out[#out + 1] = string.char(
          math.floor(value / 16777216) % 256,
          math.floor(value / 65536) % 256,
          math.floor(value / 256) % 256,
          value % 256)
        group = {}
      end
    end
  end
  if #group > 1 then
    local n = #group
    for i = n + 1, 5 do group[i] = 84 end
    local value = 0
    for _, b in ipairs(group) do value = value * 85 + (b - 33) end
    local bytes = {
      math.floor(value / 16777216) % 256,
      math.floor(value / 65536) % 256,
      math.floor(value / 256) % 256,
      value % 256,
    }
    for i = 1, n - 1 do out[#out + 1] = string.char(bytes[i]) end
  end
  return table.concat(out)
end

--- Whether a value is a stream: a dictionary with the stream's bytes.
local function is_stream(value)
  return type(value) == "table" and value.stream ~= nil
end

--- The bytes of a stream, with its filters read.
decode_stream = function(doc, dict)
  local data = dict.stream and dict.stream.data or nil
  if not data then return nil end
  local filters = get(doc, dict, "Filter")
  local names = {}
  if filters then
    if filters.kind == "name" then
      names = { filters.value }
    elseif filters.kind == "array" then
      for _, f in ipairs(filters.items) do
        local f = resolve(doc, f)
        if f and f.kind == "name" then names[#names + 1] = f.value end
      end
    end
  end
  for _, name in ipairs(names) do
    if name == "FlateDecode" or name == "Fl" then
      local out = inflate(data)
      if not out then return nil end
      data = out
    elseif name == "ASCIIHexDecode" or name == "AHx" then
      data = hex_decode(data)
    elseif name == "ASCII85Decode" or name == "A85" then
      data = ascii85_decode(data)
    else
      return nil
    end
  end
  return data
end

-- ---- the fonts ------------------------------------------------------------

--- The glyph names of the standard fonts.
local GLYPHS = {}
do
  for i = 65, 90 do GLYPHS[string.char(i)] = string.char(i) end
  for i = 97, 122 do GLYPHS[string.char(i)] = string.char(i) end
  for i = 48, 57 do GLYPHS[string.char(i)] = string.char(i) end
  local names = {
    space = " ", exclam = "!", quotedbl = "\"", numbersign = "#", dollar = "$",
    percent = "%", ampersand = "&", quotesingle = "'", quoteright = "’",
    parenleft = "(", parenright = ")", asterisk = "*", plus = "+", comma = ",",
    hyphen = "-", period = ".", slash = "/", colon = ":", semicolon = ";",
    less = "<", equal = "=", greater = ">", question = "?", at = "@",
    bracketleft = "[", backslash = "\\", bracketright = "]", asciicircum = "^",
    underscore = "_", grave = "`", braceleft = "{", bar = "|", braceright = "}",
    asciitilde = "~", quoteleft = "‘", quotedblleft = "“", quotedblright = "”",
    endash = "–", emdash = "—", bullet = "•", fi = "fi", fl = "fl",
    dagger = "†", daggerdbl = "‡", ellipsis = "…", trademark = "™",
    Euro = "€", degree = "°", plusminus = "±", multiply = "×", divide = "÷",
    copyright = "©", registered = "®", section = "§", paragraph = "¶",
    sterling = "£", yen = "¥", cent = "¢", currency = "¤", quotesinglbase = "‚",
    quotedblbase = "„", perthousand = "‰", guilsinglleft = "‹",
    guilsinglright = "›", ellipsisdot = "…", nbspace = " ", minus = "−",
    twosuperior = "²", threesuperior = "³", onesuperior = "¹", mu = "µ",
    Delta = "Δ", Omega = "Ω", pi = "π", alpha = "α", beta = "β", gamma = "γ",
  }
  for name, ch in pairs(names) do GLYPHS[name] = ch end
end

--- The character of a byte in WinAnsi.
local function win_ansi(code)
  if code >= 0x80 and code <= 0x9F then
    local high = {
      [0x80] = "€", [0x82] = "‚", [0x83] = "ƒ", [0x84] = "„",
      [0x85] = "…", [0x86] = "†", [0x87] = "‡", [0x88] = "ˆ",
      [0x89] = "‰", [0x8A] = "Š", [0x8B] = "‹", [0x8C] = "Œ",
      [0x8E] = "Ž", [0x91] = "‘", [0x92] = "’", [0x93] = "“",
      [0x94] = "”", [0x95] = "•", [0x96] = "–", [0x97] = "—",
      [0x98] = "˜", [0x99] = "™", [0x9A] = "š", [0x9B] = "›",
      [0x9C] = "œ", [0x9E] = "ž", [0x9F] = "Ÿ",
    }
    return high[code] or "?"
  end
  return string.char(code)
end

--- A code point as UTF-8.
local function utf8_char(code)
  if type(code) ~= "number" or code < 0 or code > 0x10FFFF then return "?" end
  if code < 0x80 then return string.char(code) end
  if code < 0x800 then
    return string.char(0xC0 + math.floor(code / 0x40), 0x80 + code % 0x40)
  end
  if code < 0x10000 then
    return string.char(0xE0 + math.floor(code / 0x1000),
      0x80 + math.floor(code / 0x40) % 0x40, 0x80 + code % 0x40)
  end
  return string.char(0xF0 + math.floor(code / 0x40000),
    0x80 + math.floor(code / 0x1000) % 0x40,
    0x80 + math.floor(code / 0x40) % 0x40, 0x80 + code % 0x40)
end

--- The text of a ToUnicode map's hex string.
local function utf8_of(hex)
  local units = {}
  for i = 1, #hex, 4 do units[#units + 1] = tonumber(hex:sub(i, i + 3), 16) or 0 end
  local out = {}
  local i = 1
  while i <= #units do
    local u = units[i]
    if u >= 0xD800 and u <= 0xDBFF and units[i + 1] then
      local code = 0x10000 + (u - 0xD800) * 0x400 + (units[i + 1] - 0xDC00)
      out[#out + 1] = utf8_char(code)
      i = i + 2
    else
      out[#out + 1] = utf8_char(u)
      i = i + 1
    end
  end
  return table.concat(out)
end

--- A ToUnicode CMap: the codes and the text they stand for.
local function parse_cmap(data)
  local map = {}
  local two = false
  for section in data:gmatch("begincodespacerange(.-)endcodespacerange") do
    for lo in section:gmatch("<([0-9A-Fa-f]+)>") do
      if #lo >= 4 then two = true end
    end
  end
  for section in data:gmatch("beginbfchar(.-)endbfchar") do
    for src, dst in section:gmatch("<([0-9A-Fa-f]+)>%s*<([0-9A-Fa-f]+)>") do
      map[tonumber(src, 16)] = utf8_of(dst)
    end
  end
  for section in data:gmatch("beginbfrange(.-)endbfrange") do
    for lo, hi, dst in section:gmatch("<([0-9A-Fa-f]+)>%s*<([0-9A-Fa-f]+)>%s*<([0-9A-Fa-f]+)>") do
      local first, last = tonumber(lo, 16), tonumber(hi, 16)
      local base = tonumber(dst, 16)
      if first and last and base and base <= 0x10FFFF and last >= first and last - first < 65536 then
        for code = first, last do map[code] = utf8_char(base + (code - first)) end
      end
    end
    for lo, list in section:gmatch("<([0-9A-Fa-f]+)>%s*<([0-9A-Fa-f]+)>%s*%[(.-)%]") do
      local first = tonumber(lo, 16)
      local i = 0
      for dst in list:gmatch("<([0-9A-Fa-f]+)>") do
        if first then map[first + i] = utf8_of(dst) end
        i = i + 1
      end
    end
  end
  return { map = map, two_byte = two }
end

--- What a font's codes stand for.
local function font_info(doc, font)
  local info = { unicode = {}, diffs = {} }
  local subtype = get(doc, font, "Subtype")
  info.two_byte = subtype and subtype.kind == "name" and subtype.value == "Type0"
  local encoding = get(doc, font, "Encoding")
  if encoding then
    if encoding.kind == "name" then
      info.base = encoding.value
    elseif encoding.kind == "dict" then
      local base = get(doc, encoding, "BaseEncoding")
      if base and base.kind == "name" then info.base = base.value end
      local diffs = get(doc, encoding, "Differences")
      if diffs and diffs.kind == "array" then
        local code = 0
        for _, item in ipairs(diffs.items) do
          local item = resolve(doc, item)
          if item and item.kind == "number" then
            code = item.value
          elseif item and item.kind == "name" then
            local ch = GLYPHS[item.value]
            if ch then info.diffs[code] = ch end
            code = code + 1
          end
        end
      end
    end
  end
  local tou = get(doc, font, "ToUnicode")
  if is_stream(tou) then
    local data = decode_stream(doc, tou)
    if data then
      local cmap = parse_cmap(data)
      info.unicode = cmap.map
      if cmap.two_byte then info.two_byte = true end
    end
  end
  return info
end

--- The text of a string, in the font's own codes.
local function decode_text(bytes, font)
  if not font then return bytes end
  local out = {}
  if next(font.unicode) ~= nil then
    if font.two_byte then
      local i = 1
      while i + 1 <= #bytes do
        local code = bytes:byte(i) * 256 + bytes:byte(i + 1)
        out[#out + 1] = font.unicode[code] or "?"
        i = i + 2
      end
    else
      for i = 1, #bytes do
        local code = bytes:byte(i)
        out[#out + 1] = font.unicode[code] or win_ansi(code)
      end
    end
    return table.concat(out)
  end
  if font.two_byte then return nil end
  for i = 1, #bytes do
    local code = bytes:byte(i)
    out[#out + 1] = font.diffs[code] or win_ansi(code)
  end
  return table.concat(out)
end

-- ---- the pages ------------------------------------------------------------

--- The fonts a page has, by name.
local function fonts_of(doc, page, resources)
  local out = {}
  local res = get(doc, page, "Resources") or resources
  local fonts = get(doc, res, "Font")
  if fonts and fonts.kind == "dict" then
    for name, ref in pairs(fonts.map) do
      local font = resolve(doc, ref)
      if font and font.kind == "dict" then out[name] = font_info(doc, font) end
    end
  end
  return out
end

--- The text of a content stream, with the fonts it uses.
local function content_text(doc, content, fonts)
  local out = {}
  local pos = 1
  local stack = {}
  local font = nil
  local size = 12
  local leading = 0
  local x, y = 0, 0
  local line_x, line_y = 0, 0
  local last_x, last_y = nil, nil

  local function show(bytes)
    local text = decode_text(bytes, font)
    if not text or text == "" then return end
    if last_x then
      if math.abs(y - last_y) > 0.5 then
        out[#out + 1] = "\n"
      elseif x - last_x > size * 0.2 then
        out[#out + 1] = " "
      end
    end
    out[#out + 1] = text
    -- the text is wider now (an estimate: half a character per unit)
    last_x = x + #text * size * 0.5
    last_y = y
    x = last_x
  end

  while true do
    pos = skip(content, pos)
    if pos > #content then break end
    local c = content:sub(pos, pos)
    local token, next_pos
    if c == "(" then
      token, next_pos = read_literal(content, pos + 1)
    elseif c == "<" then
      if content:sub(pos + 1, pos + 1) == "<" then
        token, next_pos = read_dict(content, pos, doc)
      else
        token, next_pos = read_hex(content, pos + 1)
      end
    elseif c == "/" then
      token, next_pos = read_name(content, pos)
    elseif c == "[" then
      token, next_pos = read_array(content, pos, doc)
    elseif c == "]" or c == "{" or c == "}" then
      pos = pos + 1
    else
      local s, e, a, b = content:find("^([+-]?%d+)%s+([+-]?%d+)%s+R", pos)
      if s then
        token, next_pos = { kind = "ref", num = tonumber(a), gen = tonumber(b) }, e + 1
      else
        local num = content:match("^[+-]?%d*%.?%d*", pos)
        if num and num:find("%d") then
          token, next_pos = { kind = "number", value = tonumber(num) }, pos + #num
        else
          local word = content:match("^[A-Za-z'\"][A-Za-z0-9*'\"%%]*", pos)
          if word then
            token, next_pos = { kind = "op", value = word }, pos + #word
          else
            pos = pos + 1
          end
        end
      end
    end
    if not token then
      pos = next_pos or (pos + 1)
    else
      pos = next_pos
      if token.kind == "op" then
        local op = token.value
        local n = #stack
        local function number(i)
          local v = stack[n - i + 1]
          return (v and v.kind == "number") and v.value or nil
        end
        if op == "BT" then
          x, y = 0, 0
          line_x, line_y = 0, 0
          last_x, last_y = nil, nil
        elseif op == "Tf" then
          local name = stack[n - 1]
          font = (name and name.kind == "name") and fonts[name.value] or nil
          size = number(1) or size
        elseif op == "TL" then
          leading = number(1) or leading
        elseif op == "Td" then
          line_x = line_x + (number(2) or 0)
          line_y = line_y + (number(1) or 0)
          x, y = line_x, line_y
        elseif op == "TD" then
          leading = -(number(1) or 0)
          line_x = line_x + (number(2) or 0)
          line_y = line_y + (number(1) or 0)
          x, y = line_x, line_y
        elseif op == "Tm" then
          line_x, line_y = number(2) or 0, number(1) or 0
          x, y = line_x, line_y
        elseif op == "T*" then
          line_y = line_y - leading
          x, y = line_x, line_y
        elseif op == "Tj" then
          local s = stack[n]
          if s and s.kind == "string" then show(s.bytes) end
        elseif op == "TJ" then
          local arr = stack[n]
          if arr and arr.kind == "array" then
            for _, item in ipairs(arr.items) do
              if item.kind == "string" then
                show(item.bytes)
              elseif item.kind == "number" then
                if item.value <= -120 then out[#out + 1] = " " end
                x = x - item.value / 1000 * size
              end
            end
          end
        elseif op == "'" then
          line_y = line_y - leading
          x, y = line_x, line_y
          local s = stack[n]
          if s and s.kind == "string" then show(s.bytes) end
        elseif op == '"' then
          line_y = line_y - leading
          x, y = line_x, line_y
          local s = stack[n]
          if s and s.kind == "string" then show(s.bytes) end
        end
        stack = {}
      else
        stack[#stack + 1] = token
      end
    end
  end
  return table.concat(out)
end

--- The text of a page.
local function page_text(doc, page, resources)
  local streams = {}
  local contents = get(doc, page, "Contents")
  if contents then
    if is_stream(contents) then
      streams[#streams + 1] = contents
    elseif contents.kind == "array" then
      for _, c in ipairs(contents.items) do
        local c = resolve(doc, c)
        if is_stream(c) then streams[#streams + 1] = c end
      end
    end
  end
  if #streams == 0 then return "" end
  local fonts = fonts_of(doc, page, resources)
  local out = {}
  for _, s in ipairs(streams) do
    local data = decode_stream(doc, s)
    if data then
      local text = content_text(doc, data, fonts)
      if text and text ~= "" then out[#out + 1] = text end
    end
  end
  return table.concat(out, "\n")
end

--- The pages of the file, in order.
local function find_pages(doc)
  local pages = {}
  local catalog
  for _, obj in pairs(doc.objects) do
    local t = get(doc, obj.value, "Type")
    if t and t.kind == "name" and t.value == "Catalog" then
      catalog = obj.value
      break
    end
  end
  local function walk(node, resources, depth)
    if depth > 32 or type(node) ~= "table" then return end
    local res = get(doc, node, "Resources") or resources
    local t = get(doc, node, "Type")
    if t and t.kind == "name" and t.value == "Page" then
      node.resources = res
      pages[#pages + 1] = node
      return
    end
    local kids = get(doc, node, "Kids")
    if kids and kids.kind == "array" then
      for _, kid in ipairs(kids.items) do
        walk(resolve(doc, kid), res, depth + 1)
      end
    end
  end
  if catalog then
    local root = get(doc, catalog, "Pages")
    if root then walk(root, nil, 0) end
  end
  if #pages == 0 then
    -- no page tree: every page there is, in the order of their numbers
    local found = {}
    for _, obj in pairs(doc.objects) do
      local t = get(doc, obj.value, "Type")
      if t and t.kind == "name" and t.value == "Page" then
        found[#found + 1] = { num = obj.num, page = obj.value }
      end
    end
    table.sort(found, function(a, b) return a.num < b.num end)
    for _, f in ipairs(found) do pages[#pages + 1] = f.page end
  end
  return pages
end

-- ---- the file -------------------------------------------------------------

--- The text of the PDF at `path`, or nil and why not.
function M.text(path)
  local file, err = io.open(path, "rb")
  if not file then return nil, tostring(err) end
  local data = file:read("*a")
  file:close()
  if not data then return nil, "the file could not be read" end
  if #data > M.MAX_BYTES then return nil, "the PDF is too big to read" end
  if data:sub(1, 5) ~= "%PDF-" then return nil, "this is not a PDF" end
  local doc = scan(data)
  local pages = find_pages(doc)
  if #pages == 0 then return nil, "the PDF has no pages in it" end
  local out = {}
  for i = 1, math.min(#pages, M.MAX_PAGES) do
    local text = page_text(doc, pages[i], pages[i].resources)
    if text and text:gsub("%s", "") ~= "" then
      out[#out + 1] = text:gsub("\r\n?", "\n")
      if #out > 1 then out[#out] = "\n" .. out[#out] end
    end
  end
  if #out == 0 then return nil, "there is no text in this PDF (it may be a scan)" end
  local text = table.concat(out, "\n")
  -- the control characters are not text
  text = text:gsub("[%z\1-\8\11\12\14-\31]", "")
  -- the space a word is placed at is not always the same width
  text = text:gsub("[ \t]+", " "):gsub("\n ", "\n"):gsub(" \n", "\n")
  if #text > M.MAX_TEXT then
    text = text:sub(1, M.MAX_TEXT) .. "\n\n(The rest of the PDF is not shown.)"
  end
  return text
end

return M
