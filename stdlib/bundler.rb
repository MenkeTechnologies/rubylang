# A minimal Bundler shim, bundled into rubylang. The real Bundler's job is to
# pin `$LOAD_PATH` to the exact gem versions from Gemfile.lock; in rubylang gem
# resolution is driven by `GEM_PATH` instead (point it at the app's
# `vendor/bundle/ruby/<ver>` and the system gems), so `require "bundler/setup"`
# has nothing to do here. `Bundler.require` normally auto-requires every gem in
# the Gemfile — a no-op here, since the frameworks a Rails app boots (railties,
# active_record, action_controller, …) `require` what they need explicitly.
module Bundler
  class GemNotFound < StandardError; end

  class << self
    attr_accessor :ui

    def setup(*)
      self
    end

    # Auto-require the Gemfile gems for the given groups. rubylang resolves gems
    # through GEM_PATH, so this is a no-op; frameworks require their own deps.
    def require(*_groups)
      nil
    end

    def root
      require "pathname"
      Pathname.new(ENV["BUNDLE_GEMFILE"] ? File.dirname(ENV["BUNDLE_GEMFILE"]) : Dir.pwd)
    end

    def bundle_path
      root.join("vendor", "bundle")
    end

    def default_gemfile
      require "pathname"
      Pathname.new(ENV["BUNDLE_GEMFILE"] || File.join(Dir.pwd, "Gemfile"))
    end

    def load
      self
    end

    def environment
      self
    end

    def with_original_env
      yield if block_given?
    end

    def with_clean_env
      yield if block_given?
    end

    def original_env
      ENV.to_h
    end

    def settings
      {}
    end

    def rubygems
      self
    end

    def definition
      self
    end

    def frozen_bundle?
      false
    end
  end
end
