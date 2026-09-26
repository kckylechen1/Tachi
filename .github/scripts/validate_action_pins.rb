#!/usr/bin/env ruby
# frozen_string_literal: true

# Supply-chain policy for tracked workflows and action manifests: remote actions
# must be audited full-SHA pins, container images must be digest-pinned, and
# tools must come from audited prebuilt installs (taiki-e/install-action with an
# exact name@version, checksum on, no fallback), never `cargo install`.
#
# Threat model: repository write access is owner-only. The cargo-install
# detection is a best-effort lint against ACCIDENTAL source installs added by an
# agent or a human. It is not a sandbox and not a defence against deliberate
# obfuscation by someone with write access; static shell analysis cannot be
# complete. Known limits, accepted by design:
# - shell aliases and functions (`alias c=cargo; c install x`) are not tracked;
# - line continuations inside heredoc bodies, and other deliberate obfuscation
#   beyond the dynamic/encoded-installer guards, are out of scope;
# - false positives in the safe direction are accepted: the text
#   "cargo install" as echo/printf data, in quoted heredoc data, or
#   `cargo install --list` is rejected. Workflows should not contain it.

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
  Pin.new("taiki-e/install-action@9983c65e42da123ff25d1f78505eb6de315aa172", "v2.87.20"),
  Pin.new("taiki-e/install-action@c295c25a8d3df7288fa86db860a4f8062bf76ad8", "releases/nextest snapshot 2026-07-25")
].freeze

# No workflow may build a tool from source with `cargo install`; every such
# command is rejected unless listed here with its exact expected count.
AUDITED_CARGO_INSTALLS = {}.freeze

# Prebuilt tool installs via taiki-e/install-action: every step must declare
# `with: tool:` as an exact name@version listed here, used exactly the listed
# number of times across all workflows and local actions, with checksums on
# and the source-build fallback disabled.
INSTALL_ACTION = "taiki-e/install-action"
AUDITED_INSTALL_ACTION_TOOLS = {
  "cargo-audit@0.22.2" => 1,
  # ci.yml rust + conformance-linux.yml rust-gate and linux-platform.
  "nextest@0.9.140" => 3
}.freeze
REQUIRED_INSTALL_ACTION_INPUTS = {
  "checksum" => "true",
  "fallback" => "none"
}.freeze

# #1998: every install-action tool must run as the exact file the audited
# install extracted, never through Cargo's external-subcommand lookup
# ($CARGO_HOME/bin, then PATH) or a PATH lookup of its binary name, which on a
# self-hosted host can resolve to an unaudited copy. Tool name (the part of
# `tool:` before `@`) => [binary, cargo subcommand]. A `run:` step may not
# start that binary by name or path, nor run `cargo <subcommand>`; it runs the
# path bound by BIND_AUDITED_TOOL_SCRIPT (or an equivalent inline assertion,
# as in conformance-linux.yml), and every call to that script must assert the
# version the install pins. Every audited tool needs an entry; there is no
# exemption.
#
# How the program of a simple command is found: leading assignments,
# redirections and reserved words are skipped, then the known wrappers
# (COMMAND_WRAPPERS, by name or path) are parsed with their option grammar. A
# known wrapper with an option this file does not model rejects the command
# (fail closed) instead of guessing which word is the program. As a backstop
# for wrappers
# outside that set (stdbuf, xargs, ...), the words `<binary> <subcommand>`
# anywhere in a simple command are rejected too. Best effort, like the
# cargo-install lint below: only top-level simple commands of `run:` text are
# inspected, not scripts it calls or strings handed to `sh -c`.
AUDITED_INSTALL_ACTION_BINARIES = {
  "cargo-audit" => ["cargo-audit", "audit"],
  "nextest" => ["cargo-nextest", "nextest"]
}.freeze
BIND_AUDITED_TOOL_SCRIPT = "bind_audited_tool.sh"

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

  def initialize(inventory, pins = AUDITED_PINS, cargo_installs = AUDITED_CARGO_INSTALLS,
                 install_tools = AUDITED_INSTALL_ACTION_TOOLS, install_binaries = AUDITED_INSTALL_ACTION_BINARIES)
    @inventory = inventory
    @pins = pins
    @cargo_installs = cargo_installs
    @install_tools = install_tools
    @install_binaries = install_binaries
    @install_tool_uses = Hash.new(0)
    @approved = {}
    @ref_sha = {}
    @pin_uses = Hash.new(0)
    @cargo_uses = Hash.new(0)
    @states = {}
    @stack = []
    @run_count = 0
    @uses_count = 0
    build_map!
    build_bound_tools!
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

    @install_tools.each do |tool, expected_count|
      actual_count = @install_tool_uses[tool]
      next if actual_count == expected_count

      raise PolicyError, "audited install-action tool count mismatch: expected=#{expected_count} actual=#{actual_count}: #{tool}"
    end
  end

  private

  def build_map!
    @pins.each do |pin|
      match = pin.value.match(/\A([[:alnum:]_.-]+\/[[:alnum:]_.-]+(?:\/[^@[:space:]]+)*)@([0-9a-f]{40})\z/)
      raise PolicyError, "invalid audited action value: #{pin.value}" unless match

      action = match[1]
      sha = match[2]
      semver_label = pin.label.match?(/\Av[0-9]+(?:\.[0-9]+){0,2}\z/)
      valid_label = if action == INSTALL_ACTION
                      semver_label || pin.label.match?(/\Areleases\/nextest snapshot [0-9]{4}-[0-9]{2}-[0-9]{2}\z/)
                    else
                      semver_label
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

  # binary => {subcommand:, version:} for every bound install-action tool.
  def build_bound_tools!
    @bound_binaries = {}
    @bound_subcommands = {}
    @install_tools.each_key do |tool|
      name, version = tool.split("@", 2)
      unless version && @install_binaries.key?(name)
        raise PolicyError, "audited install-action tool has no binary mapping: #{tool}"
      end

      binary, subcommand = @install_binaries[name]
      unless binary.is_a?(String) && subcommand.is_a?(String)
        raise PolicyError, "audited install-action tool needs a [binary, subcommand] mapping: #{tool}"
      end

      if @bound_binaries.key?(binary)
        raise PolicyError, "audited install-action tool is listed at two versions: #{binary}"
      end

      @bound_binaries[binary] = {subcommand: subcommand, version: version}
      @bound_subcommands[subcommand] = binary
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
      # Reject aliases before any key-specific check reads the mapping, so an
      # aliased uses/with/input always fails as an alias.
      node.children.each { |child| walk(child, path, lines) if child.is_a?(Psych::Nodes::Alias) }
      validate_install_action_step(node, path)
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

  # Every step that uses taiki-e/install-action must name its tool explicitly
  # (never the pin's default) as an audited exact name@version, with checksum
  # verification on and no source-build fallback.
  def validate_install_action_step(node, path)
    entries = node.children.each_slice(2).select { |key, _value| key.is_a?(Psych::Nodes::Scalar) }
    uses = entries.select { |key, _value| key.value == "uses" }
    return unless uses.any? { |_key, value| value.is_a?(Psych::Nodes::Scalar) && value.value.start_with?("#{INSTALL_ACTION}@") }
    raise PolicyError, "#{path}: #{INSTALL_ACTION} step has duplicate uses keys" if uses.length > 1

    withs = entries.select { |key, _value| key.value == "with" }
    raise PolicyError, "#{path}: #{INSTALL_ACTION} step requires an explicit with: tool/checksum/fallback" if withs.empty?
    raise PolicyError, "#{path}: #{INSTALL_ACTION} step has duplicate with keys" if withs.length > 1

    inputs_node = withs.first[1]
    raise PolicyError, "#{path}: #{INSTALL_ACTION} with must be a mapping" unless inputs_node.is_a?(Psych::Nodes::Mapping)

    inputs = {}
    inputs_node.children.each_slice(2) do |key, value|
      if key.is_a?(Psych::Nodes::Alias) || value.is_a?(Psych::Nodes::Alias)
        raise PolicyError, "#{path}: YAML aliases are forbidden in supply-chain policy files"
      end
      unless key.is_a?(Psych::Nodes::Scalar) && value.is_a?(Psych::Nodes::Scalar)
        raise PolicyError, "#{path}: #{INSTALL_ACTION} inputs must be scalar key/value pairs"
      end
      raise PolicyError, "#{path}: #{INSTALL_ACTION} input is duplicated: #{key.value}" if inputs.key?(key.value)

      inputs[key.value] = value.value
    end

    allowed = ["tool"] + REQUIRED_INSTALL_ACTION_INPUTS.keys
    unknown = inputs.keys - allowed
    raise PolicyError, "#{path}: #{INSTALL_ACTION} has unaudited inputs: #{unknown.join(',')}" unless unknown.empty?

    tool = inputs["tool"]
    unless tool && @install_tools.key?(tool)
      raise PolicyError, "#{path}: #{INSTALL_ACTION} tool is not an audited exact name@version: #{tool.inspect}"
    end
    REQUIRED_INSTALL_ACTION_INPUTS.each do |input, required|
      next if inputs[input] == required

      raise PolicyError, "#{path}: #{INSTALL_ACTION} requires #{input}: #{required} (got #{inputs[input].inspect}) for #{tool}"
    end
    @install_tool_uses[tool] += 1
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
    raise PolicyError, "#{path}: unaudited cargo install command: #{value.inspect}" if cargo_install_occurrence?(value)

    validate_bound_tool_use(value, path)
  end

  # Reserved words that may precede the program word of a simple command.
  SHELL_RESERVED_WORDS = %w[if then elif else do while until ! {].freeze
  # Wrappers whose option grammar is modelled below; each runs the program
  # named after its options. Any other option of these rejects the command.
  COMMAND_WRAPPERS = %w[sudo env command exec nohup time nice timeout].freeze
  SUDO_FLAGS = %w[-A -b -E -H -k -n -P -S --askpass --background --preserve-env --set-home --non-interactive
                  --preserve-groups --stdin].freeze
  SUDO_VALUE_OPTIONS = %w[-u -g -p -C -D -T --user --group --prompt --close-from --chdir --command-timeout].freeze
  TIMEOUT_FLAGS = %w[--preserve-status --foreground -v --verbose].freeze
  TIMEOUT_VALUE_OPTIONS = %w[-s -k --signal --kill-after].freeze
  TIMEOUT_DURATION = /\A[0-9]+(?:\.[0-9]+)?[smhd]?\z/

  # #1998: see AUDITED_INSTALL_ACTION_BINARIES.
  def validate_bound_tool_use(value, path)
    return if @bound_binaries.empty?

    joined = value.gsub(/\\\r?\n/, "")
    [joined, joined.tr("\\", "/")].uniq.each do |text|
      simple_commands(text).each do |tokens|
        subcommand = @bound_subcommands.keys.find { |sub| cargo_subcommand_invoked?(tokens, [sub]) }
        if subcommand
          raise PolicyError, "#{path}: unbound install-action tool: `cargo #{subcommand}` lets Cargo pick " \
                             "#{@bound_subcommands[subcommand]} from $CARGO_HOME/bin or PATH; run the path bound by " \
                             "#{BIND_AUDITED_TOOL_SCRIPT} instead: #{value.inspect}"
        end

        program_index = command_program_index(tokens, path, value)
        if program_index && bound_binary(tokens[program_index])
          raise PolicyError, "#{path}: unbound install-action tool: #{tokens[program_index]} is started by name or path; " \
                             "run the path bound by #{BIND_AUDITED_TOOL_SCRIPT} instead: #{value.inspect}"
        end

        # Backstop for wrappers outside COMMAND_WRAPPERS: `<binary> <subcommand>`
        # is how a bound binary is started, wherever it sits in the command.
        # The bind script's own arguments are checked by validate_bind_call.
        bind_index = tokens.index { |token| token.split("/").last == BIND_AUDITED_TOOL_SCRIPT }
        tokens[0...(bind_index || tokens.length)].each_cons(2) do |word, following|
          binary = bound_binary(word)
          next unless binary && following == @bound_binaries[binary][:subcommand]

          raise PolicyError, "#{path}: unbound install-action tool: `#{word} #{following}` starts #{binary} by name " \
                             "or path; run the path bound by #{BIND_AUDITED_TOOL_SCRIPT} instead: #{value.inspect}"
        end
        validate_bind_call(tokens, path) if bind_index
      end
    end
  end

  # The bound binary a word names by basename (any directory, optional .exe).
  def bound_binary(word)
    name = word.split("/").last.to_s.sub(/\.exe\z/i, "")
    @bound_binaries.key?(name) ? name : nil
  end

  # The bind script either marks an install (`--mark <binary>`, the step right
  # before the install) or binds it; a bind must assert the version the
  # install pins.
  def validate_bind_call(tokens, path)
    script_index = tokens.index { |token| token.split("/").last == BIND_AUDITED_TOOL_SCRIPT }
    args = tokens[(script_index + 1)..] || []
    if args.first == "--mark"
      return if args.length == 2 && @bound_binaries.key?(args[1])

      raise PolicyError, "#{path}: #{BIND_AUDITED_TOOL_SCRIPT} --mark takes exactly one audited install-action " \
                         "binary: #{tokens.join(' ').inspect}"
    end
    env_var, binary, subcommand, version = args
    expected = @bound_binaries[binary]
    unless args.length == 4 && expected
      raise PolicyError, "#{path}: #{BIND_AUDITED_TOOL_SCRIPT} must bind an audited install-action tool as " \
                         "<AUDITED_VAR> <binary> <subcommand> <version>: #{tokens.join(' ').inspect}"
    end
    unless env_var.match?(/\AAUDITED_[A-Z0-9_]+\z/) && subcommand == expected[:subcommand] && version == expected[:version]
      raise PolicyError, "#{path}: #{BIND_AUDITED_TOOL_SCRIPT} asserts #{binary} #{subcommand} #{version} but the " \
                         "audited install is #{binary} #{expected[:subcommand]} #{expected[:version]}"
    end
  end

  # Index of the word naming the program a simple command runs, or nil when it
  # runs none (`command -v x`, a bare `env`, `exec >log`). Skips assignments,
  # redirections, reserved words and COMMAND_WRAPPERS with their options. An
  # option of a known wrapper that is not modelled here raises: the parser
  # never guesses which word is the program.
  def command_program_index(tokens, path, value)
    index = 0
    while (token = tokens[index])
      if SHELL_RESERVED_WORDS.include?(token) || assignment_word?(token)
        index += 1
      elsif (width = redirection_width(token))
        index += width
      elsif COMMAND_WRAPPERS.include?(token.split("/").last)
        index = wrapper_operand_index(tokens, index, path, value)
        return nil unless index
      else
        return index
      end
    end
    nil
  end

  def assignment_word?(token)
    token.match?(/\A[A-Za-z_][A-Za-z0-9_]*(?:\[[^\]]*\])?\+?=/)
  end

  # 1 for a redirection carrying its target (`>log`, `2>&1`, `<<EOF`), 2 for a
  # bare operator whose target is the next word (`> log`), nil otherwise.
  def redirection_width(token)
    return nil unless token.match?(/\A(?:[0-9]*|&)[<>]/)

    token.match?(/\A(?:[0-9]*|&)[<>&|-]+\z/) ? 2 : 1
  end

  # Index of the first word after the options of the wrapper at
  # `tokens[index]`; nil when that wrapper runs no program. Raises on an
  # option this file does not model.
  def wrapper_operand_index(tokens, index, path, value)
    wrapper = tokens[index].split("/").last
    position = index + 1
    unmodelled = lambda do |detail|
      raise PolicyError, "#{path}: cannot tell which program `#{wrapper}` runs: #{detail}; write the command " \
                         "without the wrapper or model its grammar in validate_action_pins.rb: #{value.inspect}"
    end
    takes_value = lambda do
      unmodelled.call("option #{tokens[position].inspect} has no value") unless tokens[position + 1]
      position += 2
    end
    while (option = tokens[position])&.start_with?("-")
      if option == "--"
        position += 1
        break
      end

      case wrapper
      when "env"
        case option
        when "-i", "-", "--ignore-environment", "-0", "--null", "-v", "--debug" then position += 1
        when "-u", "--unset", "-C", "--chdir" then takes_value.call
        when /\A(?:-u|-C)./, /\A--(?:unset|chdir)=./ then position += 1
        else unmodelled.call("unmodelled option #{option.inspect}")
        end
      when "sudo"
        if SUDO_FLAGS.include?(option) || option.match?(/\A--preserve-env=./)
          position += 1
        elsif SUDO_VALUE_OPTIONS.include?(option)
          takes_value.call
        elsif option.match?(/\A-[ugpCDT]./) || option.match?(/\A--(?:user|group|prompt|close-from|chdir|command-timeout)=./)
          position += 1
        else
          unmodelled.call("unmodelled option #{option.inspect}")
        end
      when "command"
        unmodelled.call("unmodelled option #{option.inspect}") unless option.match?(/\A-[pvV]+\z/)
        # -v/-V describe the command instead of running it.
        return nil if option.match?(/[vV]/)

        position += 1
      when "exec"
        if option.match?(/\A-[cl]+\z/)
          position += 1
        elsif option == "-a"
          takes_value.call
        else
          unmodelled.call("unmodelled option #{option.inspect}")
        end
      when "time"
        unmodelled.call("unmodelled option #{option.inspect}") unless option == "-p"
        position += 1
      when "nice"
        if %w[-n --adjustment].include?(option)
          takes_value.call
        elsif option.match?(/\A(?:-n-?[0-9]+|--adjustment=-?[0-9]+|-[0-9]+)\z/)
          position += 1
        else
          unmodelled.call("unmodelled option #{option.inspect}")
        end
      when "timeout"
        if TIMEOUT_FLAGS.include?(option)
          position += 1
        elsif TIMEOUT_VALUE_OPTIONS.include?(option)
          takes_value.call
        elsif option.match?(/\A(?:-[sk].|--(?:signal|kill-after)=.)/)
          position += 1
        else
          unmodelled.call("unmodelled option #{option.inspect}")
        end
      else # nohup takes no options
        unmodelled.call("unmodelled option #{option.inspect}")
      end
    end
    if wrapper == "timeout"
      duration = tokens[position]
      unmodelled.call("expected a DURATION, got #{duration.inspect}") unless duration&.match?(TIMEOUT_DURATION)
      position += 1
    end
    tokens[position] ? position : nil
  end

  # Top-level simple commands of shell text, as token lists. Full-line
  # comments are dropped (an apostrophe in prose must not unbalance quoting).
  # `&` and `|` inside a redirection (`2>&1`, `&>log`, `>|log`) stay part of
  # the redirection word. Unparseable text yields its whitespace-split words
  # (fail toward inspection).
  def simple_commands(text)
    uncommented = text.lines.reject { |line| line.match?(/\A[[:space:]]*#/) }.join
    normalized = uncommented.gsub(/\r?\n/, " ; ")
                            .gsub(/([;()]|(?<![<>])&(?!>)|(?<!>)\|)/, ' \\1 ')
                            .gsub(/[[:space:]]+/, " ")
    tokens = begin
      Shellwords.shellsplit(normalized)
    rescue ArgumentError
      normalized.split(" ")
    end
    commands = [[]]
    tokens.each { |token| SHELL_OPERATOR_TOKENS.include?(token) ? commands << [] : commands.last << token }
    commands.reject(&:empty?)
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

  # Cargo global options that consume the following word as their value.
  CARGO_VALUE_OPTIONS = %w[--config --color --explain -Z -C].freeze
  # Cargo global options known to take no value.
  CARGO_FLAG_OPTIONS = %w[-q --quiet -v -vv -vvv --verbose --locked --frozen --offline -V --version --list -h --help].freeze
  # Subcommands that install a tool (binstall may itself fall back to a source build).
  CARGO_INSTALL_SUBCOMMANDS = %w[install binstall].freeze
  SHELL_OPERATOR_TOKENS = %w[; & | ( )].freeze

  # True when any simple command in `value` runs cargo with an install
  # subcommand, after skipping a `+toolchain` selector and global options, or
  # runs a cargo-install/cargo-binstall binary directly. Wrappers (env
  # assignments, sudo/env/command/exec, `\cargo`, POSIX or Windows paths,
  # pwsh `&`) are covered because the cargo word is matched anywhere in the
  # command; quoted strings are re-scanned as nested commands. Best effort:
  # see the threat model and known limits at the top of this file.
  def cargo_install_occurrence?(value, depth = 0)
    # Like bash, delete backslash-newline so a word split across lines rejoins.
    joined = value.gsub(/\\\r?\n/, "")
    # A second pass with `\` read as a path separator keeps unquoted Windows
    # paths (C:\Rust\bin\cargo.exe) intact through POSIX tokenization.
    [joined, joined.tr("\\", "/")].uniq.any? { |text| cargo_install_text?(text, depth) }
  end

  def cargo_install_text?(text, depth)
    normalized = text.gsub(/\r?\n/, " ; ").gsub(/([;&|()])/, ' \\1 ').gsub(/[[:space:]]+/, " ")
    tokens = Shellwords.shellsplit(normalized)
    commands = [[]]
    tokens.each { |token| SHELL_OPERATOR_TOKENS.include?(token) ? commands << [] : commands.last << token }
    return true if commands.any? { |command| cargo_install_command?(command) }

    depth < 3 && tokens.any? { |token| token.match?(/[[:space:]]/) && cargo_install_occurrence?(token, depth + 1) }
  rescue ArgumentError
    # Unparseable shell text fails closed on any cargo ... install shape.
    (normalized || text).match?(/(?:\A|[^[:alnum:]_-])cargo(?:-b?install\b|(?:\.exe)?[^;&|]*?[[:space:]]b?install(?:[[:space:]]|\z))/i)
  end

  def cargo_install_command?(tokens)
    tokens.any? { |token| token.split(%r{[/\\]}).last.to_s.match?(/\Acargo-b?install(?:\.exe)?\z/i) } ||
      cargo_subcommand_invoked?(tokens, CARGO_INSTALL_SUBCOMMANDS)
  end

  # True when any `cargo` word in `tokens` runs one of `subcommands`, after
  # skipping a `+toolchain` selector and global options.
  def cargo_subcommand_invoked?(tokens, subcommands)
    tokens.each_with_index.any? do |token, index|
      program = token.split(%r{[/\\]}).last.to_s
      next false unless program.match?(/\Acargo(?:\.exe)?\z/i)

      position = index + 1
      position += 1 if tokens[position]&.start_with?("+")
      ambiguous = false
      while (option = tokens[position]) && option.start_with?("-") && option != "--"
        if CARGO_VALUE_OPTIONS.include?(option)
          position += 2
        else
          ambiguous ||= !CARGO_FLAG_OPTIONS.include?(option) && !option.match?(/\A(?:--[a-z-]+=|-[ZC].)/)
          position += 1
        end
      end
      # An unrecognised option might take a value; then the real subcommand is one word later.
      subcommands.include?(tokens[position]) ||
        (ambiguous && subcommands.include?(tokens[position + 1]))
    end
  end

  def trailing_ref_comment(node, lines)
    return nil unless node.start_line == node.end_line

    line = lines.fetch(node.end_line, "")
    tail = line[node.end_column..] || ""
    comment = tail.match(/#[[:space:]]*(.*?)[[:space:]]*\z/)
    comment && comment[1]
  end
end

def validate_virtual(sources, pins: [], cargo_installs: {}, install_tools: {},
                     install_binaries: AUDITED_INSTALL_ACTION_BINARIES, roots: nil)
  inventory = RepoInventory.virtual(sources)
  policy = ActionPolicy.new(inventory, pins, cargo_installs, install_tools, install_binaries)
  policy.validate_roots(roots || inventory.policy_root_files)
  policy
end

def expect_rejected(name, expected)
  yield
rescue PolicyError => e
  unless e.message.match?(expected)
    raise PolicyError, "fixture rejected for the wrong reason: #{name}: expected #{expected.inspect}, got #{e.message.inspect}"
  end

  puts "fixture REJECTED #{name}"
else
  raise PolicyError, "fixture unexpectedly accepted: #{name}"
end

UNAUDITED_CARGO = /unaudited cargo install command/
DYNAMIC_INSTALLER = %r{no known dynamic/encoded installer or transformer-to-shell construction}
NOT_AUDITED_ACTION = /remote action is not in the audited mapping/
YAML_ALIAS = /YAML aliases are forbidden/

def self_test!
  wrong_sha = "0123456789abcdef0123456789abcdef01234567"
  root = ".github/workflows/root.yml"
  action = "custom/action/action.yml"

  expect_rejected("local-missing", /local action manifest is missing or untracked/) do
    validate_virtual(root => "uses: ./missing\n")
  end
  expect_rejected("local-escape", /local action escapes repository root/) do
    validate_virtual(root => "uses: ./../outside\n")
  end
  expect_rejected("local-ambiguous", /local action manifest is ambiguous/) do
    sources = {root => "uses: ./custom/action\n", action => "name: a\n", "custom/action/action.yaml" => "name: b\n"}
    validate_virtual(sources)
  end
  expect_rejected("local-cycle", /local action cycle detected/) do
    sources = {root => "uses: ./custom/a\n", "custom/a/action.yml" => "uses: ./custom/b\n", "custom/b/action.yml" => "uses: ./custom/a\n"}
    validate_virtual(sources)
  end
  expect_rejected("nested-local-remote", NOT_AUDITED_ACTION) do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - uses: unknown/action@#{wrong_sha} # v1\n"}
    validate_virtual(sources)
  end
  expect_rejected("nested-local-docker", /docker action requires an immutable sha256 digest/) do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - uses: docker://alpine:3.20\n"}
    validate_virtual(sources)
  end
  expect_rejected("root-coverage", /policy root coverage mismatch/) do
    sources = {root => "name: one\n", ".github/workflows/other.yaml" => "name: two\n"}
    validate_virtual(sources, roots: [root])
  end
  expect_rejected("root-coverage-action-omitted", /policy root coverage mismatch/) do
    sources = {root => "name: one\n", action => "name: hidden\n"}
    validate_virtual(sources, roots: [root])
  end
  expect_rejected("unreferenced-action-remote", NOT_AUDITED_ACTION) do
    sources = {root => "name: one\n", action => "uses: unknown/action@#{wrong_sha} # v1\n"}
    validate_virtual(sources)
  end
  expect_rejected("unreferenced-action-docker", /docker action requires an immutable sha256 digest/) do
    sources = {root => "name: one\n", action => "uses: docker://alpine:3.20\n"}
    validate_virtual(sources)
  end
  expect_rejected("unreferenced-action-run", DYNAMIC_INSTALLER) do
    sources = {root => "name: one\n", action => "run: cargo${EMPTY} install cargo-audit\n"}
    validate_virtual(sources)
  end

  # Production allows no cargo install at all; exercise the exact-match matcher
  # against a fixture-local audited command.
  audited = "cargo install cargo-audit --version 0.22.2 --locked --quiet"
  fixture_cargo_installs = {audited => 1}.freeze
  expect_rejected("cargo-install-source-build-retired", UNAUDITED_CARGO) do
    validate_virtual({root => "run: #{audited}\n"}, cargo_installs: AUDITED_CARGO_INSTALLS)
  end
  cargo_fixtures = {
    "cargo-env-prefix" => ["run: FOO=bar #{audited}\n", UNAUDITED_CARGO],
    "cargo-sudo" => ["run: sudo #{audited}\n", UNAUDITED_CARGO],
    "cargo-semicolon" => ["run: #{audited}; echo done\n", UNAUDITED_CARGO],
    "cargo-multiline" => ["run: |\n  #{audited}\n", UNAUDITED_CARGO],
    "cargo-continuation" => ["run: |\n  cargo \\\n  install cargo-audit --version 0.22.2 --locked --quiet\n", UNAUDITED_CARGO],
    "cargo-shell-string" => ["run: sh -c 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n", DYNAMIC_INSTALLER],
    "cargo-github-command" => ["run: ${{ env.CARGO }} install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-github-subcommand" => ["run: cargo ${{ env.SUBCOMMAND }} cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-shell-default" => ["run: ${CARGO:-cargo} install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-concatenated" => ["run: cargo${EMPTY} install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-fragment-concatenated" => ["run: ca${X}rgo in${Y}stall cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-command-substitution" => ["run: $(printf cargo) install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-backticks" => ["run: '`printf cargo` install cargo-audit --version 0.22.2 --locked --quiet'\n", DYNAMIC_INSTALLER],
    "cargo-eval" => ["run: eval 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n", UNAUDITED_CARGO],
    "cargo-bash-c" => ["run: bash -c 'cargo install cargo-audit --version 0.22.2 --locked --quiet'\n", DYNAMIC_INSTALLER],
    "cargo-assignment" => ["run: CMD=cargo; $CMD install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-assigned-command" => ["run: INSTALLER='cargo install cargo-audit --version 0.22.2 --locked --quiet'; $INSTALLER\n", DYNAMIC_INSTALLER],
    "cargo-dynamic-newline" => ["run: |\n  \"${CARGO:-cargo}\" \\\n  install cargo-audit --version 0.22.2 --locked --quiet\n", DYNAMIC_INSTALLER],
    "cargo-wrong-args" => ["run: cargo install --locked cargo-audit --version 0.22.2 --quiet\n", UNAUDITED_CARGO],
    "cargo-quoted" => ["run: \"cargo install cargo-audit --version 0.22.1 --locked --quiet\"\n", UNAUDITED_CARGO],
    "cargo-folded" => ["run: >-\n  #{audited}\n", UNAUDITED_CARGO],
    "cargo-toolchain" => ["run: cargo +stable install cargo-audit --version 0.22.2 --locked\n", UNAUDITED_CARGO],
    "cargo-toolchain-path" => ["run: /usr/local/bin/cargo +1.97.0 install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-locked" => ["run: cargo --locked install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-quiet" => ["run: cargo -q install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-short-cluster" => ["run: cargo -qv install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-config" => ["run: cargo --config net.git-fetch-with-cli=true install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-config-equals" => ["run: cargo --config=build.jobs=1 install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-color" => ["run: cargo --color never install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-unstable" => ["run: cargo -Z unstable-options install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-directory" => ["run: cargo -C /tmp install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-unknown-valued" => ["run: cargo --future-option value install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-global-mixed" => ["run: cargo +stable -v --frozen --config k=v install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-env-prefix-toolchain" => ["run: CARGO_NET_OFFLINE=false cargo +stable install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-env-command" => ["run: env RUSTFLAGS=-Cdebuginfo=0 cargo --locked install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-command-builtin" => ["run: command cargo +stable install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-backslash" => ["run: \\cargo --locked install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-exe" => ["run: cargo.exe install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-after-separator" => ["run: true;cargo install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-after-and" => ["run: cd /tmp&&cargo -q install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-subshell" => ["run: (cargo +stable install cargo-audit)\n", UNAUDITED_CARGO],
    "cargo-later-line" => ["run: |\n  echo start\n  cargo --locked install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-eval-toolchain" => ["run: eval 'cargo +stable install cargo-audit'\n", UNAUDITED_CARGO],
    "cargo-binstall-subcommand" => ["run: cargo binstall --no-confirm cargo-audit\n", UNAUDITED_CARGO],
    "cargo-binstall-binary" => ["run: cargo-binstall --no-confirm cargo-audit\n", UNAUDITED_CARGO],
    "cargo-install-binary" => ["run: ~/.cargo/bin/cargo-install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-split-word-continuation" => ["run: |\n  car\\\n  go install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-split-subcommand-continuation" => ["run: |\n  cargo ins\\\n  tall cargo-audit\n", UNAUDITED_CARGO],
    "cargo-pwsh-quoted-exe" => ["run: |\n  & \"C:\\Rust\\bin\\cargo.exe\" install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-pwsh-single-quoted-exe" => ["run: |\n  & 'C:\\Program Files\\Rust\\bin\\cargo.exe' install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-windows-unquoted-exe" => ["run: C:\\Rust\\bin\\cargo.exe install cargo-audit\n", UNAUDITED_CARGO],
    "cargo-windows-uppercase-exe" => ["run: C:\\Rust\\bin\\Cargo.EXE install cargo-audit\n", UNAUDITED_CARGO]
  }
  cargo_fixtures.each do |name, (source, expected)|
    expect_rejected(name, expected) do
      validate_virtual({root => source}, cargo_installs: fixture_cargo_installs)
    end
  end
  cargo_accepted = [
    "cargo nextest run --workspace --locked --profile ci",
    "cargo +stable build --locked",
    "cargo --config k=v --color always build",
    "cargo test -p memcore -- install",
    "cargo run --bin tool -- install",
    "echo install; cargo build",
    "& \"C:\\Rust\\bin\\cargo.exe\" build --locked",
    "C:\\Rust\\bin\\cargo.exe test -- install"
  ]
  cargo_accepted.each do |command|
    validate_virtual({root => "run: |\n  #{command}\n"}, cargo_installs: AUDITED_CARGO_INSTALLS).finish!
  end
  puts "fixture ACCEPTED cargo-non-install-subcommands (#{cargo_accepted.length})"
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
    expect_rejected(name, DYNAMIC_INSTALLER) do
      validate_virtual({root => source}, cargo_installs: fixture_cargo_installs)
    end
  end
  expect_rejected("cargo-nested-local", UNAUDITED_CARGO) do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - run: FOO=bar #{audited}\n"}
    validate_virtual(sources, cargo_installs: fixture_cargo_installs)
  end

  install_sha = "0123456789abcdef0123456789abcdef01234568"
  install_pins = [Pin.new("#{INSTALL_ACTION}@#{install_sha}", "v2.0.0")]
  install_use = "uses: #{INSTALL_ACTION}@#{install_sha} # v2.0.0"
  install_step = lambda do |with, uses: install_use|
    "steps:\n  - #{uses}\n#{with.empty? ? '' : "    with:\n#{with.map { |line| "      #{line}\n" }.join}"}"
  end
  audited_tool = "tool: cargo-audit@0.22.2"
  valid_inputs = [audited_tool, "checksum: true", "fallback: none"]
  flow_inputs = "{#{audited_tool}, checksum: true, fallback: none}"
  tool_error = /tool is not an audited exact name@version/
  fallback_error = /requires fallback: none/
  checksum_error = /requires checksum: true/
  install_fixtures = {
    "install-no-with" => [install_step.call([]), /requires an explicit with: tool\/checksum\/fallback/],
    "install-tool-unpinned-version" => [install_step.call(["tool: cargo-audit", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-latest" => [install_step.call(["tool: cargo-audit@latest", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-wrong-version" => [install_step.call(["tool: cargo-audit@0.22.1", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-extra-tool" => [install_step.call(["tool: cargo-audit@0.22.2,cargo-deny", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-space-list" => [install_step.call(["tool: cargo-audit@0.22.2 cargo-deny@0.18.0", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-expression" => [install_step.call(["tool: ${{ env.TOOL }}", "checksum: true", "fallback: none"]), tool_error],
    "install-tool-missing" => [install_step.call(["checksum: true", "fallback: none"]), tool_error],
    "install-fallback-missing" => [install_step.call([audited_tool, "checksum: true"]), fallback_error],
    "install-fallback-binstall" => [install_step.call([audited_tool, "checksum: true", "fallback: cargo-binstall"]), fallback_error],
    "install-fallback-cargo-install" => [install_step.call([audited_tool, "checksum: true", "fallback: cargo-install"]), fallback_error],
    "install-fallback-expression" => [install_step.call([audited_tool, "checksum: true", "fallback: ${{ env.FALLBACK }}"]), fallback_error],
    "install-checksum-missing" => [install_step.call([audited_tool, "fallback: none"]), checksum_error],
    "install-checksum-false" => [install_step.call([audited_tool, "checksum: false", "fallback: none"]), checksum_error],
    "install-checksum-expression" => [install_step.call([audited_tool, "checksum: ${{ env.CHECKSUM }}", "fallback: none"]), checksum_error],
    "install-unaudited-input" => [install_step.call(valid_inputs + ["extra: x"]), /has unaudited inputs: extra/],
    "install-duplicate-input" => [install_step.call([audited_tool, "tool: cargo-audit@0.22.1", "checksum: true", "fallback: none"]), /input is duplicated: tool/],
    "install-non-scalar-input" => ["steps:\n  - #{install_use}\n    with: {tool: [cargo-audit@0.22.2], checksum: true, fallback: none}\n", /inputs must be scalar key\/value pairs/],
    "install-with-not-mapping" => ["steps:\n  - #{install_use}\n    with: cargo-audit@0.22.2\n", /with must be a mapping/],
    "install-duplicate-with" => ["steps:\n  - #{install_use}\n    with: #{flow_inputs}\n    with: {tool: cargo-deny}\n", /duplicate with keys/],
    "install-duplicate-uses" => ["steps:\n  - #{install_use}\n    #{install_use}\n    with: #{flow_inputs}\n", /duplicate uses keys/],
    "install-flow-missing-fallback" => ["steps:\n  - #{install_use}\n    with: {#{audited_tool}, checksum: true}\n", fallback_error],
    "install-alias-step" => ["steps:\n  - &step\n    #{install_use}\n    with: #{flow_inputs}\n  - *step\n", YAML_ALIAS],
    "install-alias-with" => ["inputs: &inputs #{flow_inputs}\nsteps:\n  - #{install_use}\n    with: *inputs\n", YAML_ALIAS],
    "install-alias-input" => ["tool: &tool cargo-audit@0.22.2\nsteps:\n  - #{install_use}\n    with: {tool: *tool, checksum: true, fallback: none}\n", YAML_ALIAS],
    "install-alias-uses" => ["ref: &ref #{INSTALL_ACTION}@#{install_sha}\nsteps:\n  - uses: *ref\n    with: #{flow_inputs}\n", YAML_ALIAS],
    "install-merge-key" => ["base: &base {checksum: true, fallback: none}\nsteps:\n  - #{install_use}\n    with:\n      <<: *base\n      #{audited_tool}\n", YAML_ALIAS],
    "install-ref-tag" => [install_step.call(valid_inputs, uses: "uses: #{INSTALL_ACTION}@v2.0.0 # v2.0.0"), NOT_AUDITED_ACTION],
    "install-ref-refs-tags" => [install_step.call(valid_inputs, uses: "uses: #{INSTALL_ACTION}@refs/tags/v2.0.0 # v2.0.0"), NOT_AUDITED_ACTION],
    "install-ref-case" => [install_step.call(valid_inputs, uses: "uses: Taiki-E/Install-Action@#{install_sha} # v2.0.0"), NOT_AUDITED_ACTION],
    "install-ref-quoted-whitespace" => [install_step.call(valid_inputs, uses: "uses: \"#{INSTALL_ACTION}@#{install_sha} \" # v2.0.0"), NOT_AUDITED_ACTION],
    "install-ref-subpath" => [install_step.call(valid_inputs, uses: "uses: #{INSTALL_ACTION}/sub@#{install_sha} # v2.0.0"), NOT_AUDITED_ACTION],
    "install-ref-wrong-label" => [install_step.call(valid_inputs, uses: "uses: #{INSTALL_ACTION}@#{install_sha} # v2.0.1"), NOT_AUDITED_ACTION],
    "install-ref-no-label" => [install_step.call(valid_inputs, uses: "uses: #{INSTALL_ACTION}@#{install_sha}"), NOT_AUDITED_ACTION]
  }
  install_fixtures.each do |name, (source, expected)|
    expect_rejected(name, expected) do
      validate_virtual({root => source}, pins: install_pins, install_tools: AUDITED_INSTALL_ACTION_TOOLS)
    end
  end
  valid_install = install_step.call(valid_inputs)
  expect_rejected("install-nested-local-fallback", fallback_error) do
    sources = {root => "uses: ./custom/action\n",
               action => "runs:\n  using: composite\n  #{install_step.call([audited_tool, "checksum: true"]).gsub("\n", "\n  ")}"}
    validate_virtual(sources, pins: install_pins, install_tools: AUDITED_INSTALL_ACTION_TOOLS)
  end
  expect_rejected("install-count-exceeded", /audited install-action tool count mismatch: expected=1 actual=2: cargo-audit@0.22.2/) do
    doubled = valid_install + install_step.call(valid_inputs).delete_prefix("steps:\n")
    validate_virtual({root => doubled}, pins: install_pins, install_tools: {"cargo-audit@0.22.2" => 1}).finish!
  end
  expect_rejected("install-count-missing", /audited install-action tool count mismatch: expected=1 actual=0: cargo-audit@0.22.2/) do
    validate_virtual({root => "steps:\n  - run: echo ok\n"}, install_tools: {"cargo-audit@0.22.2" => 1}).finish!
  end
  one_tool = {"cargo-audit@0.22.2" => 1}

  # #1998: install-action tools run only through the bound, version-asserted
  # path. Fixtures pass the mapping explicitly, independent of the live table.
  both_bound = {"cargo-audit" => ["cargo-audit", "audit"], "nextest" => ["cargo-nextest", "nextest"]}
  unbound = /unbound install-action tool/
  bind_mismatch = /bind_audited_tool\.sh asserts .* but the audited install is/
  bind_shape = /bind_audited_tool\.sh must bind an audited install-action tool/
  bind = "bash .github/scripts/bind_audited_tool.sh"
  bound_fixtures = {
    "bound-cargo-nextest" => ["run: cargo nextest run --workspace --locked --profile ci\n", unbound],
    "bound-cargo-audit" => ["run: cargo audit --deny warnings\n", unbound],
    "bound-cargo-toolchain-options" => ["run: cargo +1.97.0 --locked nextest run\n", unbound],
    "bound-cargo-env-prefix" => ["run: |\n  set -e\n  RUST_LOG=info cargo nextest run\n", unbound],
    "bound-cargo-if" => ["run: if cargo audit; then echo ok; fi\n", unbound],
    "bound-cargo-pwsh" => ["run: |\n  & cargo nextest run\n", unbound],
    "bound-cargo-exe" => ["run: C:\\Rust\\bin\\cargo.exe audit\n", unbound],
    "bound-binary-by-name" => ["run: cargo-nextest nextest run\n", unbound],
    "bound-binary-cargo-home" => ["run: ~/.cargo/bin/cargo-nextest nextest run\n", unbound],
    "bound-binary-install-dir-unasserted" => ["run: /Users/runner/.install-action/bin/cargo-audit audit\n", unbound],
    "bound-binary-after-and-sudo" => ["run: echo start && sudo cargo-audit audit\n", unbound],
    "bound-binary-exe" => ["run: cargo-nextest.exe nextest run\n", unbound],
    "bind-wrong-version" => ["run: #{bind} AUDITED_NEXTEST cargo-nextest nextest 0.9.141\n", bind_mismatch],
    "bind-wrong-subcommand" => ["run: #{bind} AUDITED_NEXTEST cargo-nextest run 0.9.140\n", bind_mismatch],
    "bind-bad-env-var" => ["run: #{bind} NEXTEST cargo-nextest nextest 0.9.140\n", bind_mismatch],
    "bind-unknown-binary" => ["run: #{bind} AUDITED_DENY cargo-deny deny 0.18.0\n", bind_shape],
    "bind-missing-version" => ["run: #{bind} AUDITED_NEXTEST cargo-nextest nextest\n", bind_shape]
  }
  bound_fixtures.each do |name, (source, expected)|
    expect_rejected(name, expected) do
      validate_virtual({root => source}, install_tools: AUDITED_INSTALL_ACTION_TOOLS, install_binaries: both_bound)
    end
  end
  expect_rejected("bound-nested-local", unbound) do
    sources = {root => "uses: ./custom/action\n", action => "runs:\n  using: composite\n  steps:\n    - run: cargo nextest run\n"}
    validate_virtual(sources, install_tools: AUDITED_INSTALL_ACTION_TOOLS, install_binaries: both_bound)
  end
  # astra r1 finding 1: wrappers are parsed with their option grammar, so an
  # option is never mistaken for the program; an unmodelled option of a known
  # wrapper rejects the command; `<binary> <subcommand>` behind any other
  # wrapper is caught by the backstop.
  unmodelled = /cannot tell which program `[a-z]+` runs/
  started = /unbound install-action tool: \S*cargo-(?:nextest|audit) is started by name or path/
  backstop = /unbound install-action tool: `\S*cargo-(?:nextest|audit) (?:nextest|audit)` starts/
  wrapper_fixtures = {
    "wrapper-command-dashdash" => ["command -- cargo-nextest nextest run", started],
    "wrapper-env-unset" => ["env -u RUST_LOG cargo-nextest nextest run", started],
    "wrapper-nice-n" => ["nice -n 10 cargo-nextest nextest run", started],
    "wrapper-timeout-cargo" => ["timeout 60 cargo nextest run", unbound],
    "wrapper-env-assignment-cargo" => ["env FOO=1 cargo nextest run", unbound],
    "wrapper-env-assignment-binary" => ["env -i PATH=/usr/bin FOO=1 cargo-audit audit", started],
    "wrapper-timeout-options" => ["timeout --signal=KILL -k 5 5m cargo-nextest nextest run", started],
    "wrapper-sudo-user" => ["sudo -n -u runner -E cargo-audit audit --deny warnings", started],
    "wrapper-exec-argv0" => ["exec -a x cargo-nextest nextest run", started],
    "wrapper-chain" => ["time -p nohup nice -5 env -C /tmp -- cargo-nextest nextest run", started],
    "wrapper-redirect-first" => [">log.txt 2>&1 cargo-nextest nextest run", started],
    "wrapper-redirect-split" => ["2> err.log cargo-audit audit", started],
    "wrapper-unmodelled-stdbuf" => ["stdbuf -oL cargo-nextest nextest run", backstop],
    "wrapper-unmodelled-xargs" => ["echo run | xargs ~/.cargo/bin/cargo-audit audit", backstop],
    "wrapper-env-split-string" => ["env -S 'cargo-nextest nextest run'", unmodelled],
    "wrapper-by-path-split-string" => ["/usr/bin/env -S 'cargo-nextest nextest run'", unmodelled],
    "wrapper-nice-unknown-option" => ["nice --foo cargo-nextest nextest run", unmodelled],
    "wrapper-command-unknown-option" => ["command -x cargo-nextest", unmodelled],
    "wrapper-timeout-no-duration" => ["timeout cargo-nextest nextest run", unmodelled],
    "wrapper-time-unknown-option" => ["time -f %e cargo-nextest nextest run", unmodelled],
    "wrapper-sudo-shell" => ["sudo -s cargo-nextest nextest run", unmodelled],
    "wrapper-option-without-value" => ["env -u", unmodelled],
    "bind-mark-unknown-binary" => ["#{bind} --mark cargo-deny", /--mark takes exactly one audited install-action binary/],
    "bind-mark-extra-argument" => ["#{bind} --mark cargo-nextest nextest",
                                   /--mark takes exactly one audited install-action binary/]
  }
  wrapper_fixtures.each do |name, (command, expected)|
    expect_rejected(name, expected) do
      validate_virtual({root => "run: |\n  #{command}\n"}, install_tools: AUDITED_INSTALL_ACTION_TOOLS,
                                                         install_binaries: both_bound)
    end
  end
  expect_rejected("bound-unmapped-tool", /audited install-action tool has no binary mapping: cargo-deny@0.18.0/) do
    validate_virtual({root => "name: one\n"}, install_tools: {"cargo-deny@0.18.0" => 1})
  end
  expect_rejected("bound-nil-mapping", /needs a \[binary, subcommand\] mapping: cargo-audit@0.22.2/) do
    validate_virtual({root => "name: one\n"}, install_tools: AUDITED_INSTALL_ACTION_TOOLS,
                                              install_binaries: both_bound.merge("cargo-audit" => nil))
  end
  # The live policy binds every audited tool: no exemption remains.
  expect_rejected("bound-live-nextest", unbound) do
    validate_virtual({root => "run: cargo nextest run\n"}, install_tools: AUDITED_INSTALL_ACTION_TOOLS)
  end
  expect_rejected("bound-live-cargo-audit", unbound) do
    validate_virtual({root => "run: cargo audit --deny warnings\n"}, install_tools: AUDITED_INSTALL_ACTION_TOOLS)
  end
  bound_accepted = [
    "\"${AUDITED_NEXTEST:?}\" nextest run --workspace --locked --profile ci",
    "\"${AUDITED_CARGO_AUDIT:?}\" audit --deny warnings",
    "#{bind} AUDITED_NEXTEST cargo-nextest nextest 0.9.140",
    "#{bind} AUDITED_CARGO_AUDIT cargo-audit audit 0.22.2",
    "#{bind} --mark cargo-nextest",
    "#{bind} --mark cargo-audit",
    "required_tools=(cargo rustc cargo-nextest cargo-audit python3)",
    "echo 'cargo nextest is bound' && type -P cargo-nextest",
    "printf 'cargo-audit %s (fake)' 0.22.2 >\"${dir}/cargo-audit\"",
    "cargo test --workspace --locked --doc",
    "command -v cargo-nextest",
    "command -pv cargo-audit",
    "local -a auth_env=(env -u GITHUB_TOKEN -u GH_TOKEN)",
    "exec >\"${GITHUB_STEP_SUMMARY}\" 2>&1",
    "timeout 60 \"${AUDITED_NEXTEST:?}\" nextest run",
    "env RUST_LOG=info \"${AUDITED_NEXTEST:?}\" nextest run",
    "nice -n 10 -- \"${AUDITED_CARGO_AUDIT:?}\" audit",
    "sudo apt-get install -y gcc-aarch64-linux-gnu",
    "env | grep -oE '^CARGO_ALIAS_' || true"
  ]
  bound_accepted.each do |command|
    validate_virtual({root => "run: |\n  #{command}\n"}, install_tools: AUDITED_INSTALL_ACTION_TOOLS,
                                                       install_binaries: both_bound)
  end
  puts "fixture ACCEPTED bound-install-action-tools (#{bound_accepted.length})"

  validate_virtual({root => valid_install}, pins: install_pins, install_tools: one_tool).finish!
  validate_virtual({root => "steps:\n  - #{install_use}\n    with: #{flow_inputs}\n"}, pins: install_pins, install_tools: one_tool).finish!
  quoted = "steps:\n  - \"uses\": #{INSTALL_ACTION}@#{install_sha} # v2.0.0\n    'with':\n      \"tool\": cargo-audit@0.22.2\n      checksum: \"true\"\n      fallback: 'none'\n"
  validate_virtual({root => quoted}, pins: install_pins, install_tools: one_tool).finish!
  anchored = "steps:\n  - &step\n    #{install_use}\n    with: #{flow_inputs}\n"
  validate_virtual({root => anchored}, pins: install_pins, install_tools: one_tool).finish!
  puts "fixture ACCEPTED install-action-block install-action-flow install-action-quoted install-action-unused-anchor"

  digest = "a" * 64
  image_error = /container image requires an immutable sha256 digest/
  docker_fixtures = {
    "docker-container-comment-digest" => ["jobs:\n  test:\n    container: alpine:3.20 # @sha256:#{digest}\n", image_error],
    "docker-container-quoted-tag" => ["jobs: {test: {container: \"alpine:3.20\"}}\n", image_error],
    "docker-container-flow-image-tag" => ["jobs: {test: {container: {image: \"alpine:3.20\"}}}\n", image_error],
    "docker-service-image-tag" => ["jobs:\n  test:\n    services: {db: {image: \"postgres:16\"}}\n", image_error],
    "docker-folded-image-tag" => ["jobs:\n  test:\n    container:\n      image: >-\n        alpine:3.20\n", image_error],
    "docker-action-image-tag" => ["runs:\n  using: docker\n  image: alpine:3.20\n", image_error],
    "docker-uses-comment-digest" => ["uses: docker://alpine:3.20 # @sha256:#{digest}\n", /docker action requires an immutable sha256 digest/]
  }
  docker_fixtures.each do |name, (source, expected)|
    expect_rejected(name, expected) do
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
  valid = validate_virtual(valid_sources, cargo_installs: fixture_cargo_installs)
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
puts "supply-policy OK roots=#{ARGV.length} workflows=#{inventory.workflow_files.length} actions=#{inventory.action_manifest_files.length} uses=#{policy.uses_count} runs=#{policy.run_count} audited_actions=#{AUDITED_PINS.length} audited_install_tools=#{AUDITED_INSTALL_ACTION_TOOLS.length}"
