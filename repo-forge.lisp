;; repos.lisp — authoritative catalog of pleme-io repositories.
;;
;; Each (defrepo …) form declares one repo. `repo-forge check
;; --catalog repos.lisp` verifies the fleet is coherent with its spec.
;; `repo-forge migrate --catalog repos.lisp` reconciles drift.
;;
;; Sorted alphabetically by name. One blank line between entries.

(defrepo arnes
  :description "Rust/tatara-lisp open-source Claude-Code-equivalent agent harness"
  :kind :rust-workspace-tool
  :visibility :private
  :binary "arnes"
  :package-name "arnes"
  :workspace-members ("arnes-core" "arnes-anthropic" "arnes-mcp-client"
                      "arnes-loop" "arnes-tui" "arnes-config" "arnes")
  :siblings
    ((:name tatara :crate-name tatara-lisp        :path "tatara-lisp"        :kind :path)
     (:name tatara :crate-name tatara-lisp-derive :path "tatara-lisp-derive" :kind :path)
     (:name shikumi                                                           :kind :path))
  :external-crates
    ((:name "serde"      :version "1"    :features ("derive"))
     (:name "serde_json" :version "1")
     (:name "anyhow"     :version "1")
     (:name "thiserror"  :version "2")
     (:name "clap"       :version "4"    :features ("derive" "env"))
     (:name "schemars"   :version "0.8"  :features ("chrono"))
     (:name "reqwest"    :version "0.12" :features ("json" "rustls-tls") :default-features #f)
     (:name "tokio"      :version "1"    :features ("macros" "rt-multi-thread" "io-std" "io-util" "sync" "fs" "process"))
     (:name "tracing"    :version "0.1")
     (:name "tracing-subscriber" :version "0.3" :features ("env-filter"))
     (:name "ratatui"    :version "0.29")
     (:name "crossterm"  :version "0.28")
     (:name "async-trait" :version "0.1")
     (:name "futures"    :version "0.3")
     (:name "chrono"     :version "0.4"  :features ("serde"))
     (:name "pretty_assertions" :version "1")
     (:name "tempfile"   :version "3"))
  :hm-module
    (:namespace "programs"
     :settings
       ((:name "model"        :type :str             :default "claude-opus-4-7"
         :description "Anthropic model ID arnes should use by default.")
        (:name "theme"        :type (:enum ("dark" "light" "nord"))
         :default "dark"      :description "TUI theme.")
        (:name "maxTokens"    :type :int             :default 8192
         :description "Default max_tokens for Anthropic requests.")
        (:name "systemPrompt" :type :null-or-str     :default null
         :description "Optional system prompt injected at conversation start.")
        (:name "endpoint"     :type :null-or-str     :default null
         :description "Override the Anthropic base URL (e.g. for proxies)."))
     :render-shikumi-config #t)
  :ci
    (:systems       ("aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux")
     :test-systems  ("aarch64-darwin")
     :build-images  #f))

(defrepo escriba
  :description "Rust + tatara-lisp editor, built on the pleme-io GPU stack"
  :kind :rust-workspace-tool
  :visibility :private
  :binary "escriba"
  :package-name "escriba"
  :siblings
    ((:name tatara  :crate-name tatara-lisp        :path "tatara-lisp"        :kind :path)
     (:name tatara  :crate-name tatara-lisp-derive :path "tatara-lisp-derive" :kind :path)
     (:name shikumi                                                            :kind :path)
     (:name garasu                                                             :kind :path)
     (:name madori                                                             :kind :path)
     (:name awase                                                              :kind :path)
     (:name hayai                                                              :kind :path)
     (:name irodori                                                            :kind :path)
     (:name mojiban                                                            :kind :path))
  :hm-module
    (:namespace "programs"
     :settings
       ((:name "theme"       :type :str             :default "nord"
         :description "Color theme — Nord by default.")
        (:name "tabWidth"    :type :int             :default 2
         :description "Display width of a tab character.")
        (:name "lineNumbers" :type :bool            :default #t
         :description "Show absolute line numbers in the gutter.")
        (:name "statusline"  :type :bool            :default #t
         :description "Render the bottom status line.")
        (:name "initialMode" :type (:enum ("normal" "insert" "visual" "command"))
         :default "normal"   :description "Mode escriba starts in.")
        (:name "render"      :type (:enum ("gpu" "tui" "text"))
         :default "tui"      :description "Default render backend."))
     :render-shikumi-config #t)
  :ci
    (:systems       ("aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux")
     :test-systems  ("aarch64-darwin")
     :build-images  #f))

(defrepo repo-forge
  :description "typescape-driven substrate-native renderer for pleme-io repositories"
  :kind :rust-workspace-tool
  :visibility :private
  :binary "repo-forge"
  :package-name "repo-forge-cli"
  :siblings
    ((:name tatara :crate-name tatara-lisp        :path "tatara-lisp"        :kind :path)
     (:name tatara :crate-name tatara-lisp-derive :path "tatara-lisp-derive" :kind :path))
  :external-crates
    ((:name "serde"      :version "1"    :features ("derive"))
     (:name "serde_json" :version "1")
     (:name "serde_yaml" :version "0.9")
     (:name "toml"       :version "0.8")
     (:name "toml_edit"  :version "0.22")
     (:name "thiserror"  :version "2")
     (:name "anyhow"     :version "1")
     (:name "clap"       :version "4"    :features ("derive" "env"))
     (:name "schemars"   :version "0.8"  :features ("chrono"))
     (:name "indexmap"   :version "2"    :features ("serde"))
     (:name "blake3"     :version "1")
     (:name "hex"        :version "0.4")
     (:name "tracing"    :version "0.1")
     (:name "tracing-subscriber" :version "0.3" :features ("env-filter")))
  :hm-module
    (:namespace "programs"
     :settings
       ((:name "catalogPath" :type :null-or-str     :default null
         :description "Path to the fleet's repos.lisp catalog. Used by tend post-hooks."))
     :render-shikumi-config #f)
  :ci
    (:systems       ("aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux")
     :test-systems  ("aarch64-darwin")
     :build-images  #f))
