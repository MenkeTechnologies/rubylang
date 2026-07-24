# delegate — object delegation, a pure-Ruby port of Ruby's stdlib `delegate`,
# bundled into rubylang and loaded by `require "delegate"`. Provides the
# `Delegator` base, `SimpleDelegator`, and the `DelegateClass` factory.

# Abstract base: forwards missing methods to the object returned by __getobj__.
class Delegator
  def initialize(obj)
    __setobj__(obj)
  end

  def __getobj__
    raise NotImplementedError, "#{self.class}#__getobj__ is not implemented"
  end

  def __setobj__(_obj)
    raise NotImplementedError, "#{self.class}#__setobj__ is not implemented"
  end

  def method_missing(m, *args, &block)
    target = __getobj__
    if target.respond_to?(m)
      target.__send__(m, *args, &block)
    else
      super
    end
  end

  def respond_to_missing?(m, include_private = false)
    __getobj__.respond_to?(m, include_private) || super
  end

  def ==(other)
    return true if other.equal?(self)
    __getobj__ == other
  end

  def !=(other)
    return false if other.equal?(self)
    __getobj__ != other
  end

  def !
    !__getobj__
  end

  def to_s
    __getobj__.to_s
  end

  def inspect
    __getobj__.inspect
  end
end

# A ready-to-use delegator that stores its target in an ivar.
class SimpleDelegator < Delegator
  def __getobj__
    @delegate_sd_obj
  end

  def __setobj__(obj)
    @delegate_sd_obj = obj
  end
end

# `DelegateClass(superklass)` returns a Delegator subclass that forwards every
# public instance method of `superklass` to the wrapped object. Subclass it:
# `class Foo < DelegateClass(Bar)`.
def DelegateClass(superklass, &block)
  klass = Class.new(Delegator)
  methods = superklass.public_instance_methods
  methods -= [:__getobj__, :__setobj__, :==, :!=, :!, :to_s, :inspect]
  methods -= ::Delegator.public_instance_methods
  klass.class_eval do
    def __getobj__
      @delegate_dc_obj
    end

    def __setobj__(obj)
      @delegate_dc_obj = obj
    end

    methods.each do |method|
      define_method(method) do |*args, &blk|
        __getobj__.__send__(method, *args, &blk)
      end
    end
  end
  klass.class_eval(&block) if block
  klass
end
