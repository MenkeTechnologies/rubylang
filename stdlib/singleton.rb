# singleton — a port of Ruby's stdlib Singleton mixin, bundled into rubylang and
# loaded by `require "singleton"`. `include Singleton` gives a class a single,
# lazily-created instance reachable through `.instance` (Rails' Mime types use it).

module Singleton
  # When mixed in, the including class gains the `instance` class method.
  def self.included(klass)
    klass.extend(SingletonClassMethods)
  end

  module SingletonClassMethods
    # The one instance, created on first access and memoized on the class.
    def instance
      @singleton_instance ||= new
    end

    # A subclass of a singleton is itself a singleton.
    def inherited(sub)
      super
      sub.instance_variable_set(:@singleton_instance, nil)
    end
  end

  # A singleton instance cannot be copied — both return self is not correct, so
  # Ruby raises; mirror that.
  def clone
    raise TypeError, "can't clone instance of singleton #{self.class}"
  end

  def dup
    raise TypeError, "can't dup instance of singleton #{self.class}"
  end
end
