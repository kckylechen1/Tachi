#!/usr/bin/env ruby
# frozen_string_literal: true

require "psych"

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
  Pin.new("softprops/action-gh-release@3bb12739c298aeb8a4eeaf626c5b8d85266b0e65", "v2.6.2"),
  Pin.new("taiki-e/install-action@c295c25a8d3df7288fa86db860a4f8062bf76ad8", "releases/nextest snapshot 2026-07-25")
].freeze

class ActionPolicy
  attr_reader :uses_count

  def initialize(pins = AUDITED_PINS)
    @pins = pins
    @approved = {}
    @ref_sha = {}
    @uses = Hash.new(0)
    @uses_count = 0
    build_map!
  end

  def validate_source(source, path)
    @source_lines = source.lines
    stream = Psych.parse_stream(source, path)
    walk(stream, path)
  rescue Psych::SyntaxError => e
    raise PolicyError, "#{path}: invalid YAML: #{e.message}"
  ensure
    @source_lines = nil
  end

  def validate_file(path)
    validate_source(File.read(path), path)
  end

  def finish!
    stale = @pins.reject { |pin| @uses[[pin.value, pin.label]].positive? }
    return if stale.empty?

    pin = stale.first
    raise PolicyError, "audited action mapping is stale and unused: #{pin.value} # #{pin.label}"
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

  def walk(node, path)
    case node
    when Psych::Nodes::Alias
      raise PolicyError, "#{path}: YAML aliases are forbidden in supply-chain policy files"
    when Psych::Nodes::Mapping
      node.children.each_slice(2) do |key, value|
        walk(key, path)
        if key.is_a?(Psych::Nodes::Scalar) && key.value == "uses"
          unless value.is_a?(Psych::Nodes::Scalar)
            raise PolicyError, "#{path}: uses must have a scalar value"
          end
          validate_use(value, path)
        else
          walk(value, path)
        end
      end
    when Psych::Nodes::Stream, Psych::Nodes::Document, Psych::Nodes::Sequence
      node.children.each { |child| walk(child, path) }
    end
  end

  def validate_use(node, path)
    value = node.value
    @uses_count += 1

    if value.match?(/\A\.\/[^[:space:]]+\z/)
      return
    end

    if value.start_with?("docker://")
      unless value.match?(/\Adocker:\/\/[^@[:space:]]+@sha256:[0-9a-f]{64}\z/)
        raise PolicyError, "#{path}: docker action requires an immutable sha256 digest: #{value}"
      end
      return
    end

    label = trailing_ref_comment(node)
    key = [value, label]
    unless @approved.key?(key)
      rendered_label = label ? " # #{label}" : ""
      raise PolicyError, "#{path}: remote action is not in the audited mapping: #{value}#{rendered_label}"
    end
    @uses[key] += 1
  end

  def trailing_ref_comment(node)
    return nil unless node.start_line == node.end_line

    line = @source_lines.fetch(node.end_line, "")
    tail = line[node.end_column..] || ""
    comment = tail.match(/#[[:space:]]*(.*?)[[:space:]]*\z/)
    comment && comment[1]
  end
end

def expect_rejected(name, source, pins = AUDITED_PINS)
  begin
    policy = ActionPolicy.new(pins)
    policy.validate_source(source, "fixture:#{name}")
    policy.finish!
  rescue PolicyError
    puts "fixture REJECTED #{name}"
    return
  end
  raise PolicyError, "fixture unexpectedly accepted: #{name}"
end

def self_test!
  good_checkout = AUDITED_PINS.find { |pin| pin.value.start_with?("actions/checkout@fbc6") }
  good_nextest = AUDITED_PINS.find { |pin| pin.value.start_with?("taiki-e/install-action@") }
  wrong_sha = "0123456789abcdef0123456789abcdef01234567"

  expect_rejected("quoted-key", %Q{"uses": actions/checkout@#{wrong_sha} # v5\n})
  expect_rejected("flow-mapping", %Q{step: {uses: actions/checkout@#{wrong_sha}} # v5\n})
  expect_rejected("folded-scalar", %Q{uses: >-\n  unknown/action@#{wrong_sha}\n})
  expect_rejected("nested-composite", %Q{runs:\n  using: composite\n  steps:\n    - uses: unknown/action@#{wrong_sha} # v1\n})
  expect_rejected("wrong-sha", %Q{uses: actions/checkout@#{wrong_sha} # v5\n})
  expect_rejected("wrong-label", "uses: #{good_checkout.value} # v5.0.0\n")
  expect_rejected("unknown-action", %Q{uses: unknown/action@#{wrong_sha} # v1\n})
  expect_rejected("mutable-ref", "uses: actions/checkout@v5 # v5\n")
  expect_rejected("docker-tag", "uses: docker://alpine:3.20\n")
  expect_rejected("alias", "base: &pin\n  uses: ./local-action\ncopy: *pin\n")

  duplicate_pins = [good_checkout, Pin.new(good_checkout.value, good_checkout.label)]
  expect_rejected("duplicate-map", "uses: ./local-action\n", duplicate_pins)
  conflicting_pins = [good_checkout, Pin.new("actions/checkout@#{wrong_sha}", good_checkout.label)]
  expect_rejected("conflicting-map", "uses: ./local-action\n", conflicting_pins)
  expect_rejected("stale-map", "uses: ./local-action\n", [good_checkout])

  valid = ActionPolicy.new([good_checkout, good_nextest])
  valid.validate_source("on: push\nsteps:\n  - \"uses\": ./local-action\n  - uses: #{good_checkout.value} # #{good_checkout.label}\n  - uses: #{good_nextest.value} # #{good_nextest.label}\n  - uses: docker://alpine@sha256:#{'a' * 64}\n", "fixture:valid")
  valid.finish!
  raise PolicyError, "valid fixture did not enumerate every uses key" unless valid.uses_count == 4

  puts "fixture ACCEPTED yaml-1.1-on quoted-key local remote nextest docker-digest"
end

self_test! if ARGV.delete("--self-test")
raise PolicyError, "no policy files supplied" if ARGV.empty?

policy = ActionPolicy.new
ARGV.each { |path| policy.validate_file(path) }
policy.finish!
puts "action-policy OK files=#{ARGV.length} uses=#{policy.uses_count} audited=#{AUDITED_PINS.length}"
