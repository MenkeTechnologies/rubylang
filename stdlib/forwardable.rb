# forwardable — method delegation mixin, a pure-Ruby port of Ruby's stdlib
# `forwardable`, bundled into rubylang and loaded by `require "forwardable"`.
# A class `extend`s Forwardable, then declares which of its methods forward to an
# accessor (an ivar `:@name` or a reader method `:name`).

module Forwardable
  # Define `ali` to forward to `accessor.method(*args, &block)`. `accessor` is
  # spliced as *code* (MRI behavior): an ivar (`:@buffer`), a reader method
  # (`:buffer`), or an arbitrary expression (`'self.class'`).
  def def_instance_delegator(accessor, method, ali = method)
    accessor = accessor.to_s
    method = method.to_sym
    module_eval(
      "def #{ali}(*args, &block)\n" \
      "  #{accessor}.__send__(#{method.inspect}, *args, &block)\n" \
      "end"
    )
  end
  alias def_delegator def_instance_delegator

  # Forward each of `methods` to `accessor`, keeping the same name.
  def def_instance_delegators(accessor, *methods)
    methods.each do |method|
      next if method == :__send__ || method == :__id__
      def_instance_delegator(accessor, method)
    end
  end
  alias def_delegators def_instance_delegators

  # `delegate method => accessor` / `delegate [m1, m2] => accessor`.
  def delegate(hash)
    hash.each do |methods, accessor|
      Array(methods).each { |method| def_instance_delegator(accessor, method) }
    end
  end
  alias instance_delegate delegate
end

# SingleForwardable delegates on a single object (its singleton), rather than on
# every instance of a class.
module SingleForwardable
  def def_single_delegator(accessor, method, ali = method)
    accessor = accessor.to_s
    method = method.to_sym
    instance_eval(
      "def #{ali}(*args, &block)\n" \
      "  #{accessor}.__send__(#{method.inspect}, *args, &block)\n" \
      "end"
    )
  end
  alias def_delegator def_single_delegator

  def def_single_delegators(accessor, *methods)
    methods.each { |method| def_single_delegator(accessor, method) }
  end
  alias def_delegators def_single_delegators

  def single_delegate(hash)
    hash.each do |methods, accessor|
      Array(methods).each { |method| def_single_delegator(accessor, method) }
    end
  end
  alias delegate single_delegate
end
