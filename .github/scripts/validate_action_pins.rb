#!/usr/bin/env ruby
# frozen_string_literal: true

require "open3"
require "pathname"
require "psych"
require "set"
require "shellwords"

class PolicyError < StandardError; end

Pin = Struct.new(:value, :label)

AUDITED_PINS = [
  Pin.new("actions/checkout@08c6903cd8c0fde910a37f88322edcfb5dd907a8", "v5.0.0"),
  Pin.new("actions/checkout@11d5960a326750d5838078e36cf38b85af677262", "v4.4.0"),
  Pin.new("actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd", "v5.0.1"),
  Pin.new("actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09", "v5"),
  Pin.new("actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c", "v8.0.1"),
  Pin.new("actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093", "v4.3.0"),
  Pin.new("actions/github-script@f28e40c7f34bde8b3046d885e986cb6290c5673b", "v7.1.0"),
  Pin.new("actions/setup-node@a0853c24544627f65ddf259abe73b1d18a591444", "v5.0.0"),
  Pin.new("actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1", "v6.3.0"),
  Pin.new("actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a", "v7.0.1"),
  Pin.new("actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02", "v4.6.2"),
  Pin.new("gitleaks/gitleaks-action@e0c47f4f8be36e29cdc102c57e68cb5cbf0e8d1e", "v3.0.0"),
  Pin.new("softprops/action-gh-release@3bb12739c298aeb8a4eeaf626c5b8d85266b0e65", "v2.6.2"),
  Pin.new("taiki-e/install-action@c295c25a8d3df7288fa86db860a4f8062bf76ad8", "releases/nextest snapshot 2026-07-25")
].freeze

AUDITED_CARGO_INSTALLS = {
  "cargo install cargo-audit --version 0.22.2 --locked --quiet" => 1
}.freeze

IMMUTABLE_IMAGE = /\A[^@[:space:]]+@sha256:[0-9a-f]{64}\z/

class RepoInventory
  attr_reader :root, :tracked_files

  def self.actual(root)
    root = Pathname.new(root).expand_path
    stdout, stderr, status = Open3.capture3("git", "-C", root.to_s, "ls-files", "-z")
    raise PolicyError, "git ls-files failed: #{stderr.strip}" unless status.success?

    new(root, stdout.split("\0").to_set)
  end

  def self.virtual(sources)
    new(Pathname.new("/virtual-repo"), sources.keys.to_set, sources)
  end

  def initialize(root, tracked_files, sources = nil)
    @root = root
    @root_real = sources ? root : root.realpath
    @tracked_files = tracked_files
    @sources = sources
  end

  def workflow_files
    tracked_files.grep(%r{\A\.github/workflows/.+\.ya?ml\z}).sort
  end

  def action_manifest_files
    tracked_files.grep(%r{(?:\A|/)action\.ya?ml\z}).sort
  end

  def policy_root_files
    (workflow_files + action_manifest_files).uniq.sort
  end

  def read(relative)
    return @sources.fetch(relative) if @sources

    File.read(root.join(relative))
  rescue KeyError, Errno::ENOENT
    raise PolicyError, "tracked policy file is missing: #{relative}"
  end

  def local_manifest(value)
    relative_dir = Pathname.new(value.delete_prefix("./")).cleanpath
    if relative_dir.absolute? || relative_dir.to_s == ".." || relative_dir.to_s.start_with?("../")
      raise PolicyError, "local action escapes repository root: #{value}"
    end

    candidates = %w[action.yml action.yaml].map { |name| relative_dir.join(name).to_s }
    manifests = candidates.select { |path| tracked_files.include?(path) }
    raise PolicyError, "local action manifest is missing or untracked: #{value}" if manifests.empty?
    raise PolicyError, "local action manifest is ambiguous: #{value}" if manifests.length > 1

    manifest = manifests.first
    unless @sources
      absolute = root.join(manifest)
      raise PolicyError, "tracked local action manifest is missing: #{manifest}" unless absolute.file?
      raise PolicyError, "local action manifest must not be a symlink: #{manifest}" if absolute.symlink?

      real = absolute.realpath
      root_prefix = "#{@root_real}#{File::SEPARATOR}"
      unless real.to_s.start_with?(root_prefix)
        raise PolicyError, "local action manifest resolves outside repository: #{manifest}"
      end
    end
    manifest
  end
end

class ActionPolicy
  attr_reader :run_count, :uses_count

  def initialize(inventory, pins = AUDITED_PINS, cargo_installs = AUDITED_CARGO_INSTALLS)
    @inventory = inventory
    @pins = pins
    @cargo_installs = cargo_installs
    @approved = {}
    @ref_sha = {}
    @pin_uses = Hash.new(0)
    @cargo_uses = Hash.new(0)
    @states = {}
    @stack = []
    @run_count = 0
    @uses_count = 0
    build_map!
  end

  def validate_roots(roots)
    normalized = roots.map { |path| Pathname.new(path).cleanpath.to_s }
    raise PolicyError, "workflow roots contain duplicates" unless normalized.uniq.length == normalized.length

    expected = @inventory.policy_root_files
    missing = expected - normalized
    extra = normalized - expected
    unless missing.empty? && extra.empty?
      raise PolicyError, "policy root coverage mismatch: missing=#{missing.join(',')} extra=#{extra.join(',')}"
    end

    normalized.sort.each { |path| parse_path(path) }
  end

  def finish!
    stale_pin = @pins.find { |pin| @pin_uses[[pin.value, pin.label]].zero? }
    if stale_pin
      raise PolicyError, "audited action mapping is stale and unused: #{stale_pin.value} # #{stale_pin.label}"
    end

    @cargo_installs.each do |command, expected_count|
      actual_count = @cargo_uses[command]
      next if actual_count == expected_count

      raise PolicyError, "audited cargo install count mismatch: expected=#{expected_count} actual=#{actual_count}: #{command}"
    end
  end

  private

  def build_map!
    @pins.each do |pin|
      match = pin.value.match(/\A([[:alnum:]_.-]+\/[[:alnum:]_.-]+(?:\/[^@[:space:]]+)*)@([0-9a-f]{40})\z/)
      raise PolicyError, "invalid audited action value: #{pin.value}" unless match

      action = match[1]
      sha = match[2]
      valid_label = if action == "taiki-e/install-action"
                      pin.label.match?(/\Areleases\/nextest snapshot [0-9]{4}-[0-9]{2}-[0-9]{2}\z/)
                    else
                      pin.label.match?(/\Av[0-9]+(?:\.[0-9]+){0,2}\z/)
                    end
      raise PolicyError, "invalid audited ref label: #{pin.label}" unless valid_label

      exact_key = [pin.value, pin.label]
      ref_key = [action, pin.label]
      raise PolicyError, "duplicate audited action mapping: #{pin.value} # #{pin.label}" if @approved.key?(exact_key)
      if @ref_sha.key?(ref_key) && @ref_sha[ref_key] != sha
        raise PolicyError, "conflicting audited action mapping: #{action} # #{pin.label}"
      end

      @approved[exact_key] = true
      @ref_sha[ref_key] = sha
    end
  end

  def parse_path(path)
    case @states[path]
    when :done
      return
    when :visiting
      cycle = (@stack + [path]).join(" -> ")
      raise PolicyError, "local action cycle detected: #{cycle}"
    end

    @states[path] = :visiting
    @stack << path
    source = @inventory.read(path)
    lines = source.lines
    stream = Psych.parse_stream(source, path)
    walk(stream, path, lines)
    @states[path] = :done
  rescue Psych::SyntaxError => e
    raise PolicyError, "#{path}: invalid YAML: #{e.message}"
  ensure
    @stack.pop if @stack.last == path
  end

  def walk(node, path, lines)
    case node
    when Psych::Nodes::Alias
      raise PolicyError, "#{path}: YAML aliases are forbidden in supply-chain policy files"
    when Psych::Nodes::Mapping
      node.children.each_slice(2) do |key, value|
        walk(key, path, lines)
        if key.is_a?(Psych::Nodes::Scalar) && key.value == "uses"
          require_scalar!(value, path, "uses")
          validate_use(value, path, lines)
        elsif key.is_a?(Psych::Nodes::Scalar) && key.value == "run"
          require_scalar!(value, path, "run")
          validate_run(value, path)
        elsif key.is_a?(Psych::Nodes::Scalar) && key.value == "container"
          validate_container(value, path)
          walk(value, path, lines)
        elsif key.is_a?(Psych::Nodes::Scalar) && key.value == "services"
          validate_services(value, path)
          walk(value, path, lines)
        elsif key.is_a?(Psych::Nodes::Scalar) && key.value == "image"
          require_scalar!(value, path, "image")
          validate_image(value.value, path)
        else
          walk(value, path, lines)
        end
      end
    when Psych::Nodes::Stream, Psych::Nodes::Document, Psych::Nodes::Sequence
      node.children.each { |child| walk(child, path, lines) }
    end
  end

  def require_scalar!(node, path, key)
    return if node.is_a?(Psych::Nodes::Scalar)

    raise PolicyError, "#{path}: #{key} must have a scalar value"
  end

  def validate_container(node, path)
    if node.is_a?(Psych::Nodes::Scalar)
      validate_image(node.value, path)
      return
    end
    unless node.is_a?(Psych::Nodes::Mapping)
      raise PolicyError, "#{path}: container must be an image scalar or mapping"
    end

    image_keys = node.children.each_slice(2).select do |key, _value|
      key.is_a?(Psych::Nodes::Scalar) && key.value == "image"
    end
    raise PolicyError, "#{path}: container mapping requires image" if image_keys.empty?
  end

  def validate_services(node, path)
    raise PolicyError, "#{path}: services must be a mapping" unless node.is_a?(Psych::Nodes::Mapping)

    node.children.each_slice(2) do |_service_name, service|
      raise PolicyError, "#{path}: service must be a mapping with image" unless service.is_a?(Psych::Nodes::Mapping)

      has_image = service.children.each_slice(2).any? do |key, _value|
        key.is_a?(Psych::Nodes::Scalar) && key.value == "image"
      end
      raise PolicyError, "#{path}: service mapping requires image" unless has_image
    end
  end

  def validate_image(value, path)
    return if value.match?(IMMUTABLE_IMAGE)

    raise PolicyError, "#{path}: container image requires an immutable sha256 digest: #{value.inspect}"
  end

  def validate_use(node, path, lines)
    value = node.value
    @uses_count += 1

    if value.start_with?("./")
      manifest = @inventory.local_manifest(value)
      parse_path(manifest)
      return
    end

    if value.start_with?("docker://")
      unless value.delete_prefix("docker://").match?(IMMUTABLE_IMAGE)
        raise PolicyError, "#{path}: docker action requires an immutable sha256 digest: #{value}"
      end
      return
    end

    label = trailing_ref_comment(node, lines)
    key = [value, label]
    unless @approved.key?(key)
      rendered_label = label ? " # #{label}" : ""
      raise PolicyError, "#{path}: remote action is not in the audited mapping: #{value}#{rendered_label}"
    end
    @pin_uses[key] += 1
  end

  def validate_run(node, path)
    value = node.value
    @run_count += 1

    if node.start_line == node.end_line && @cargo_installs.key?(value)
      @cargo_uses[value] += 1
      return
    end

    if encoded_cargo_installer?(value) || dynamic_cargo_installer?(value)
      raise PolicyError, "#{path}: no known dynamic/encoded installer or transformer-to-shell construction: #{value.inspect}"
    end
    return unless cargo_install_occurrence?(value)

    raise PolicyError, "#{path}: unaudited cargo install command: #{value.inspect}"
  end

  def encoded_cargo_installer?(value)
    ansi_expanded = expand_ansi_c_strings(value)
    return true if transformer_to_shell?(ansi_expanded)

    if ansi_expanded != value && cargo_install_occurrence?(decode_shell_escapes(ansi_expanded))
      return true
    end

    if shell_c_command?(value)
      decoded = decode_shell_escapes(ansi_expanded)
      return true if cargo_install_occurrence?(decoded)
    end
  end

  def expand_ansi_c_strings(value)
    value.gsub(/\$'((?:\\.|[^'])*)'/m) { decode_shell_escapes(Regexp.last_match(1)) }
  end

  def decode_shell_escapes(value)
    value.gsub(/\\(?:x[0-9a-fA-F]{1,2}|[0-7]{1,3}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[abefnrtv\\'\"])/) do |escape|
      case escape
      when /\A\\x([0-9a-fA-F]{1,2})\z/
        [Regexp.last_match(1).to_i(16)].pack("C")
      when /\A\\([0-7]{1,3})\z/
        [Regexp.last_match(1).to_i(8)].pack("C")
      when /\A\\u([0-9a-fA-F]{4})\z/, /\A\\U([0-9a-fA-F]{8})\z/
        [Regexp.last_match(1).to_i(16)].pack("U")
      else
        {
          "\\a" => "\a", "\\b" => "\b", "\\e" => "\e", "\\f" => "\f", "\\n" => "\n",
          "\\r" => "\r", "\\t" => "\t", "\\v" => "\v", "\\\\" => "\\", "\\'" => "'", '\\"' => '"'
        }.fetch(escape)
      end
    end
  rescue RangeError
    value
  end

  def shell_c_command?(value)
    value.match?(%r{(?:\A|[;&|()[:space:]])(?:[^;&|()[:space:]]*/)?(?:bash|sh)[[:space:]]+-[A-Za-z]*c[A-Za-z]*(?:[[:space:]]|\z)})
  end

  def transformer_to_shell?(value)
    transformer = %r{(?:\A|[;&|()[:space:]])(?:[^;&|()[:space:]]*/)?(?:printf|base64|xxd)(?:[[:space:];&|()]|\z)}
    command_text = value.gsub(/\\\r?\n/, " ")
    command_text.split(/[;\r\n]+/).any? do |segment|
      next false unless segment.match?(transformer)

      piped_to_shell = segment.match?(%r{\|[[:space:]]*(?:[^;&|()[:space:]]*/)?(?:bash|sh)(?:[[:space:];&|()]|\z)})
      eval_sink = segment.match?(%r{(?:\A|[;&|()[:space:]])(?:[^;&|()[:space:]]*/)?eval(?:[[:space:]]|\z)})
      command_substitution = segment.match?(/\$\([^)]*(?:printf|base64|xxd)[^)]*\)/) ||
        segment.match?(/`[^`]*(?:printf|base64|xxd)[^`]*`/)
      piped_to_shell || shell_c_command?(segment) || eval_sink || command_substitution
    end
  end

  def dynamic_cargo_installer?(value)
    command_text = value.gsub(/\\\r?\n/, " ").gsub(/\r?\n/, " ; ")
    normalized = command_text.gsub(/[[:space:]]+/, " ")
    return false unless normalized.match?(/cargo|install|audit/)

    dynamic = /\$\{\{|\$\{|\$[A-Za-z_][A-Za-z0-9_]*|\$\(|`/
    dynamic_token = /\$\{\{.*?\}\}|\$\{.*?\}|\$[A-Za-z_][A-Za-z0-9_]*|\$\([^)]*\)|`[^`]*`/
    if normalized.match?(dynamic_token)
      static_fragments = normalized.gsub(dynamic_token, "")
      return true if static_fragments.match?(/\bcargo[[:space:]]+install\b/)
    end

    dynamic_cargo_command = /\A[[:space:]]*(?:(?:sudo|env)[[:space:]]+)?(?:[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]+[[:space:]]+)*["']?(?:\$\{\{[^}]*cargo[^}]*\}\}|\$\{[^}]*cargo[^}]*\}|\$CARGO\b|\$\([^)]*cargo[^)]*\)|`[^`]*cargo[^`]*`)/i
    segments = normalized.split(/[;&|]+/)
    return true if segments.any? do |segment|
      match = segment.match(dynamic_cargo_command)
      next false unless match

      tail = segment[match.end(0)..] || ""
      tail.match?(/install/i) || tail.match?(dynamic)
    end
    return true if segments.any? { |segment| segment.match?(/\bcargo(?![A-Za-z0-9_])[^[:space:]]*(?:\$\{\{|\$\{|\$[A-Za-z_]|\$\(|`)/) }
    return true if segments.any? { |segment| segment.match?(/\bcargo[[:space:]]+["']?(?:\$\{\{|\$\{|\$[A-Za-z_]|\$\(|`)/) }
    return true if segments.any? { |segment| segment.match?(/\bcargo\b/) && segment.match?(/install/) && segment.match?(dynamic) }
    return true if segments.any? do |segment|
      segment.match?(/(?:\A|[()[:space:]])(?:eval|sh|bash)[[:space:]]+-c(?:[[:space:]]|\z)/) &&
        segment.match?(/cargo.*install/)
    end

    segments.each_with_index.any? do |segment, index|
      assignment = segment.match(/\A[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)=(.*)\z/)
      next false unless assignment && assignment[2].match?(/cargo|install|audit/i)

      variable = Regexp.escape(assignment[1])
      later = segments[(index + 1)..].join(";")
      later.match?(/(?:\A|;)[[:space:]]*["']?\$(?:\{#{variable}\}|#{variable})\b/i) ||
        later.match?(/(?:eval|sh[[:space:]]+-c|bash[[:space:]]+-c)[[:space:]]+["']?\$(?:\{#{variable}\}|#{variable})\b/i) ||
        later.match?(/\$(?:\{#{variable}\}|#{variable})\b[^;]*install/i)
    end
  end

  def cargo_install_occurrence?(value)
    normalized = value.gsub(/\\\r?\n/, " ").gsub(/[[:space:]]+/, " ")
    tokens = Shellwords.shellsplit(normalized)
    tokens.each_cons(2).any? { |command, argument| File.basename(command) == "cargo" && argument == "install" } ||
      tokens.any? { |token| token.match?(/\bcargo[[:space:]]+install\b/) }
  rescue ArgumentError
    normalized.match?(%r{(?:\A|[;&|()[:space:]])(?:[^[:space:];&|()]*/)?cargo[[:space:]]+install(?:[[:space:]]|\z)})
  end

  def trailing_ref_comment(node, lines)
    return nil unless node.start_line == node.end_line

    line = lines.fetch(node.end_line, "")
    tail = line[node.end_column..] || ""
    comment = tail.match(/#[[:space:]]*(.*?)[[:space:]]*\z/)
    comment && comment[1]
  end
end

def validate_virtual(sources, pins: [], cargo_installs: {}, roots: nil)
  inventory = RepoInventory.virtual(sources)
  policy = ActionPolicy.new(inventory, pins, cargo_installs)
  policy.validate_roots(roots || inventory.policy_root_files)
  policy
end

def expect_rejected(name)
  yield
rescue PolicyError
  puts "fixture REJECTED #{name}"
else
  raise PolicyError, "fixture unexpectedly accepted: #{name}"
end

def self_test!
  wrong_sha = "0123456789abcdef0123456789abcdef01234567"
  root = ".github/workflows/root.yml"
  action = "custom/action/action.yml"

  expect_rejected("local-missing") do
    validate_virtual(root => "uses: ./missing\n")
  end
  expect_rejected("local-escape") do
    validate_virtual(root => "uses: ./../outside\n")
  end
  expect_rejected("local-ambiguous") do
    sources = {root => "uses: ./custom/action\n", action => "name: a\n", "custom/action/action.yaml" => "name: b\n"}
    validate_virtual(sources)
  end
  expect_rejected("local-cycle") do
    sources = {root => "uses: ./custom/a\n", "custom/a/action.yml" => "uses: ./custom/b\n", "custom/b/action.yml" => "uses: ./custom/a\n"}
    validate_virtual(sources)
  end
  expect_rejected("nested-local-remote") do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - uses: unknown/action@#{wrong_sha} # v1\n"}
    validate_virtual(sources)
  end
  expect_rejected("nested-local-docker") do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - uses: docker://alpine:3.20\n"}
    validate_virtual(sources)
  end
  expect_rejected("root-coverage") do
    sources = {root => "name: one\n", ".github/workflows/other.yaml" => "name: two\n"}
    validate_virtual(sources, roots: [root])
  end
  expect_rejected("root-coverage-action-omitted") do
    sources = {root => "name: one\n", action => "name: hidden\n"}
    validate_virtual(sources, roots: [root])
  end
  expect_rejected("unreferenced-action-remote") do
    sources = {root => "name: one\n", action => "uses: unknown/action@#{wrong_sha} # v1\n"}
    validate_virtual(sources)
  end
  expect_rejected("unreferenced-action-docker") do
    sources = {root => "name: one\n", action => "uses: docker://alpine:3.20\n"}
    validate_virtual(sources)
  end
  expect_rejected("unreferenced-action-run") do
    sources = {root => "name: one\n", action => "run: cargo${EMPTY} install cargo-audit\n"}
    validate_virtual(sources)
  end

  audited = AUDITED_CARGO_INSTALLS.keys.first
  cargo_fixtures = {
    "cargo-env-prefix" => "run: FOO=bar #{audited}\n",
    "cargo-sudo" => "run: sudo #{audited}\n",
    "cargo-semicolon" => "run: #{audited}; echo done\n",
    "cargo-multiline" => "run: |\n  #{audited}\n",
    "cargo-continuation" => "run: |\n  cargo \\\n  install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-shell-string" => "run: sh -c 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n",
    "cargo-github-command" => "run: ${{ env.CARGO }} install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-github-subcommand" => "run: cargo ${{ env.SUBCOMMAND }} cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-shell-default" => "run: ${CARGO:-cargo} install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-concatenated" => "run: cargo${EMPTY} install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-fragment-concatenated" => "run: ca${X}rgo in${Y}stall cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-command-substitution" => "run: $(printf cargo) install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-backticks" => "run: '`printf cargo` install cargo-audit --version 0.22.2 --locked --quiet'\n",
    "cargo-eval" => "run: eval 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n",
    "cargo-bash-c" => "run: bash -c 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n",
    "cargo-assignment" => "run: CMD=cargo; $CMD install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-assigned-command" => "run: INSTALLER='cargo install cargo-audit --version 0.22.2 --locked --quiet'; $INSTALLER\n",
    "cargo-dynamic-newline" => "run: |\n  \"${CARGO:-cargo}\" \\\n+  install cargo-audit --version 0.22.2 --locked --quiet\n",
    "cargo-wrong-args" => "run: cargo install --locked cargo-audit --version 0.22.2 --quiet\n",
    "cargo-quoted" => "run: \"cargo install cargo-audit --version 0.22.1 --locked --quiet\"\n",
    "cargo-folded" => "run: >-\n  #{audited}\n"
  }
  cargo_fixtures.each do |name, source|
    expect_rejected(name) do
      validate_virtual({root => source}, cargo_installs: AUDITED_CARGO_INSTALLS)
    end
  end
  encoded_fixtures = {
    "cargo-ansi-c" => "run: |\n  $'\\x63\\x61\\x72\\x67\\x6f' $'\\x69\\x6e\\x73\\x74\\x61\\x6c\\x6c' cargo-audit\n",
    "cargo-ansi-c-split" => "run: |\n  $'ca'$'rgo' $'in'$'stall' cargo-audit\n",
    "cargo-ansi-c-inline-split" => "run: |\n  ca$'rg'o in$'st'all cargo-audit\n",
    "cargo-printf-hex" => "run: |\n  printf '\\x63\\x61\\x72\\x67\\x6f\\x20\\x69\\x6e\\x73\\x74\\x61\\x6c\\x6c cargo-audit' | sh\n",
    "cargo-printf-octal" => "run: |\n  printf '\\143\\141\\162\\147\\157\\040\\151\\156\\163\\164\\141\\154\\154 cargo-audit' | bash\n",
    "cargo-printf-escaped" => "run: |\n  printf 'cargo\\040install cargo-audit' | /bin/sh\n",
    "cargo-printf-percent-b" => "run: |\n  printf '%b' '\\x63\\x61\\x72\\x67\\x6f\\x20install cargo-audit' | bash\n",
    "cargo-bash-c-ansi" => "run: |\n  bash -c $'\\x63\\x61\\x72\\x67\\x6f\\x20install cargo-audit'\n",
    "cargo-sh-c-encoded-printf" => "run: |\n  sh -c \"$(printf '\\x63\\x61\\x72\\x67\\x6f\\x20install cargo-audit')\"\n",
    "cargo-base64-pipe" => "run: |\n  printf 'Y2FyZ28gaW5zdGFsbCBjYXJnby1hdWRpdA==' | base64 --decode | sh\n",
    "cargo-xxd-pipe" => "run: |\n  printf '636172676f20696e7374616c6c20636172676f2d6175646974' | xxd -r -p | bash\n",
    "printf-path-hex" => "run: |\n  /usr/bin/printf '\\x65\\x63\\x68\\x6f ok' | /bin/sh\n",
    "base64-split-payload" => "run: |\n  printf '%s' 'ZWNoby' 'Bvaw==' | /usr/bin/base64 --decode | bash\n",
    "xxd-split-payload" => "run: |\n  printf '%s' '6563686f' '206f6b' | /usr/bin/xxd -r -p | sh\n",
    "transformer-shell-c" => "run: |\n  bash -c \"$(printf echo)\"\n",
    "transformer-eval" => "run: |\n  eval \"$(base64 --decode <<< ZWNobyBvaw==)\"\n",
    "transformer-command-substitution" => "run: |\n  $(xxd -r -p <<< 6563686f)\n"
  }
  encoded_fixtures.each do |name, source|
    expect_rejected(name) do
      validate_virtual({root => source}, cargo_installs: AUDITED_CARGO_INSTALLS)
    end
  end
  expect_rejected("cargo-nested-local") do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - run: FOO=bar #{audited}\n"}
    validate_virtual(sources, cargo_installs: AUDITED_CARGO_INSTALLS)
  end

  digest = "a" * 64
  docker_fixtures = {
    "docker-container-comment-digest" => "jobs:\n  test:\n    container: alpine:3.20 # @sha256:#{digest}\n",
    "docker-container-quoted-tag" => "jobs: {test: {container: \"alpine:3.20\"}}\n",
    "docker-container-flow-image-tag" => "jobs: {test: {container: {image: alpine:3.20}}}\n",
    "docker-service-image-tag" => "jobs:\n  test:\n    services: {db: {image: postgres:16}}\n",
    "docker-folded-image-tag" => "jobs:\n  test:\n    container:\n      image: >-\n        alpine:3.20\n",
    "docker-action-image-tag" => "runs:\n  using: docker\n  image: alpine:3.20\n",
    "docker-uses-comment-digest" => "uses: docker://alpine:3.20 # @sha256:#{digest}\n"
  }
  docker_fixtures.each do |name, source|
    expect_rejected(name) do
      validate_virtual(root => source)
    end
  end

  valid_docker = <<~YAML
    jobs:
      scalar:
        container: "alpine@sha256:#{digest}"
        services: {db: {image: "postgres@sha256:#{digest}"}}
      mapping:
        container:
          image: >-
            ubuntu@sha256:#{digest}
        steps:
          - uses: docker://alpine@sha256:#{digest}
  YAML
  validate_virtual(root => valid_docker)
  puts "fixture ACCEPTED docker-digests docker-uses scalar-container mapping-container service-image quoted-flow-folded"

  valid_sources = {
    root => "on: push\nsteps:\n  - \"uses\": ./custom/action\n  - run: #{audited}\n",
    action => "runs:\n  using: composite\n  steps:\n    - run: echo ok\n"
  }
  valid = validate_virtual(valid_sources, cargo_installs: AUDITED_CARGO_INSTALLS)
  valid.finish!
  raise PolicyError, "valid fixture inventory mismatch" unless valid.uses_count == 1 && valid.run_count == 2
  puts "fixture ACCEPTED complete-roots nested-local exact-cargo-install yaml-1.1-on"
end

run_self_test = ARGV.delete("--self-test")
repo_root_index = ARGV.index("--repo-root")
repo_root = repo_root_index ? ARGV.delete_at(repo_root_index + 1) : Dir.pwd
ARGV.delete_at(repo_root_index) if repo_root_index
self_test! if run_self_test
raise PolicyError, "no policy roots supplied" if ARGV.empty?

inventory = RepoInventory.actual(repo_root)
policy = ActionPolicy.new(inventory)
policy.validate_roots(ARGV)
policy.finish!
puts "supply-policy OK roots=#{ARGV.length} workflows=#{inventory.workflow_files.length} actions=#{inventory.action_manifest_files.length} uses=#{policy.uses_count} runs=#{policy.run_count} audited_actions=#{AUDITED_PINS.length}"
