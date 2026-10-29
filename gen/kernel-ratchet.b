use("hadome", %i[failures measurement orphans parse render shrunk])
use("kyohi", %i[enforce refusal])
use("moji", %i[ends_with includes lines starts_with trim])
use("retsu", %i[drop_while enumerate first size take_while])

def root()
  getenv("GEN_ROOT", ".")
end

def at(p)
  path_join(root, p)
end

def members()
  decl = first(
    filter(
      (l) -> starts_with(trim(l), "members"),
      lines(read_file(at("Cargo.toml")))
    )
  )
  map(
    (ix) -> ix[1],
    filter((ix) -> first(ix) % 2 == 1, enumerate(split(decl, "\"")))
  )
end

def counted(l)
  t = trim(l)
  t != "" and not starts_with(t, "//")
end

def body(text)
  take_while((l) -> trim(l) != "#[cfg(test)]", lines(text))
end

def code_lines(text)
  size(filter(counted, body(text)))
end

def crate_lines(c)
  files = filter((p) -> ends_with(p, ".rs"), walk_dir(at("#{c}/src")))
  reduce((acc, p) -> acc + code_lines(read_file(p)), 0, files)
end

def host_block(ls)
  take_while(
    (l) -> l != "}",
    drop_while((l) -> not starts_with(l, "#[blue_lang_host::host"), ls)
  )
end

def host_functions()
  ls = body(read_file(at("arnes-blue/src/lib.rs")))
  methods = size(filter((l) -> starts_with(trim(l), "fn "), host_block(ls)))
  methods + size(filter((l) -> includes(l, "register_fn(\"arnes_"), ls))
end

def unmeasured()
  map(
    fn(m)
      refusal(
        :not_an_input,
        "workspace member #{m} is not an input of the kernel ratchet: add it to the Bluefile's generate inputs and give it a kernel-baseline.txt row"
      )
    end,
    filter((m) -> not is_dir?(at(m)), members)
  )
end

def advise(rows, kind, ms)
  map(
    fn(m)
      write_stderr(
        unlines(
          [
            "kernel-ratchet: #{kind} #{m[:key]} measures below its row; lower it in kernel-baseline.txt"
          ]
        )
      )
    end,
    shrunk(rows, kind, ms)
  )
  map(
    fn(k)
      write_stderr(
        unlines(
          [
            "kernel-ratchet: #{kind} #{k} has a row and nothing to measure; drop the row"
          ]
        )
      )
    end,
    orphans(rows, kind, map((m) -> m[:key], ms))
  )
end

enforce(unmeasured, [(fs) -> fs], {kind: :first})
rows = parse(read_file(at("kernel-baseline.txt")))
rust = map((m) -> measurement(m, crate_lines(m)), members)
host = [measurement("arnes-blue", host_functions)]

enforce(
  append(failures(rows, "rust", rust), failures(rows, "host", host)),
  [(fs) -> fs],
  {kind: :first}
)

advise(rows, "rust", rust)
advise(rows, "host", host)

write_file(
  getenv("GEN_OUT", "docs/kernel-ratchet.txt"),
  "#{render("host", host)}#{render("rust", rust)}"
)
