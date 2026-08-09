# Regenerates `src/arity_table.rs` — the declarative shapes (arity, parameters)
# of MRI's built-in methods, keyed by the module that DEFINES each one, and the
# range of argument counts each one actually accepts.
#
# A built-in has no written parameter list rubylang can inspect, so `Method#arity`
# / `#owner` / `#parameters` on one can only be answered from a table. The table
# is dumped straight out of the reference interpreter rather than hand-written,
# so it says exactly what MRI says.
#
#   ruby tools/gen_arity_table.rb > src/arity_table.rs && cargo fmt
#
# Rows come from the instance-method surface (including inherited, so the owner
# columns fill in Kernel/Comparable/Enumerable/...) plus the singleton surface of
# the core classes. Default-gem owners (did_you_mean, error_highlight) are
# skipped: they are not core semantics and a stripped install does not have them.
#
# `BUILTIN_ARG_SHAPES` is MEASURED, not derived. `Method#arity` cannot express
# what a C function accepts: `String#center` reports -1 yet takes 1..2, and
# `Object#send` reports -1 yet demands at least one argument. The only source of
# truth is what the reference interpreter raises, so this script CALLS each
# method with a deliberately wrong number of arguments and reads the count range
# out of the `ArgumentError` it raises.
#
# Two properties keep that safe. A probe far above any real maximum (`HIGH`)
# raises before the body runs, so nothing is executed for a method with a bounded
# maximum; and the zero-argument probe — the only one that can run a body — is
# reached only when the high probe proved the method takes a rest argument, and
# never for a name in `UNSAFE`. A row that cannot be measured is simply omitted:
# the guard consuming this table checks only what was measured, so an omission
# costs coverage and can never cost correctness.
#
# This script must load NO stdlib. A `require` adds methods the core interpreter
# does not define (`Kernel#j` from json, `Dir.mktmpdir` from tmpdir), and those
# would land in the table as though MRI defined them.

CLASSES = %w[
  BasicObject Object Kernel Comparable Enumerable
  Integer Float Numeric Rational Complex
  String Symbol Array Hash Range
  NilClass TrueClass FalseClass
  Proc Method UnboundMethod Module Class
  Exception StandardError ArgumentError RuntimeError TypeError NameError
  NoMethodError ZeroDivisionError IndexError KeyError StopIteration FrozenError
  IOError SystemExit LocalJumpError RangeError NotImplementedError
  Regexp MatchData Struct Time IO File Dir
  Enumerator Enumerator::Lazy Enumerator::Yielder
  Thread Thread::Mutex Thread::Queue Thread::SizedQueue Thread::ConditionVariable
  Random Data Set Binding Fiber Encoding Math Marshal Process GC ObjectSpace
  Signal FileTest Warning
]

# Owners injected by default gems rather than by the core interpreter.
SKIP_OWNER = /\A(DidYouMean|ErrorHighlight)\b/

# Method names never probed. Calling one — even with arguments it will reject —
# can take the machine, the terminal or this script somewhere it should not go:
# it can exit, fork, block forever, consume stdin, or delete a file. Omitting a
# row only costs the guard some coverage, so this list stays generous.
UNSAFE = %w[
  fork exec exec! system spawn syscall exit exit! abort at_exit trap daemon
  sleep gets readline readlines readpartial read_nonblock sysread readbyte readchar
  wait wait2 waitall waitpid waitpid2 kill setsid setpgid setpgrp setrlimit
  detach stop run wakeup terminate raise
  popen reopen close close_read close_write fcntl ioctl flock ftruncate
  chdir chroot mkdir rmdir unlink rename chmod chown lchmod lchown symlink link
  write putc puts syswrite write_nonblock pos= seek rewind
  start compact disable enable garbage_collect
  set_trace_func add_trace_func irb
  loop cycle catch callcc
  srand test open
  # `Module#freeze` would freeze the very Comparable/String objects later rows
  # are probed on, so every measurement after it would be against a changed
  # receiver.
  freeze
  # `pp` autoloads Ruby's pp library, which does `Object.include(PP::ObjectMixin)`
  # and so rewrites the ancestor chain of EVERY class in the table.
  pp
  suspend transfer resume
].freeze

# Unsafe only on a particular owner. A blanket ban on these names costs real
# coverage — `join` is `Thread#join`, which blocks, but it is also `Array#join`,
# which is one of the most-called methods in the language — so each one is banned
# exactly where it is dangerous and probed everywhere else.
UNSAFE_PAIR = [
  # Re-initialises the running thread and takes the interpreter down with it.
  ["Thread", "initialize"],
  ["Thread", "join"], ["Thread", "value"],
  # Blocks until a descriptor is ready.
  ["#<Class:IO>", "select"],
  # Deletes or resizes files.
  ["#<Class:File>", "delete"], ["#<Class:Dir>", "delete"], ["#<Class:IO>", "delete"],
  ["IO", "truncate"], ["File", "truncate"], ["#<Class:File>", "truncate"],
  # Write to the terminal or a file rather than to a value.
  ["IO", "print"], ["IO", "printf"], ["IO", "write"],
  ["#<Class:IO>", "print"], ["#<Class:IO>", "printf"], ["#<Class:IO>", "write"],
].freeze

# Methods that FORWARD their arguments somewhere else, so the count they accept
# belongs to the target, not to them, and no single measurement describes it.
# `Class#new` hands its arguments to the receiver's `initialize`: probing it on
# `String` measures `String#initialize`, and a table row saying `new` takes 0..1
# then rejects `Range.new(1, 3)`, `Date.new(2024, 2, 29)` and every two-argument
# constructor in the language. A row left out here is a row nothing checks, which
# is exactly right for a method whose shape is not its own.
UNMEASURABLE = [
  ["Class", "new"],
  ["Proc", "call"], ["Proc", "()"], ["Proc", "[]"], ["Proc", "yield"], ["Proc", "==="],
  ["Method", "call"], ["Method", "()"], ["Method", "[]"], ["Method", "==="],
  ["UnboundMethod", "bind_call"],
  ["Enumerator::Yielder", "yield"], ["Enumerator::Yielder", "<<"],
  ["Enumerator::Yielder", "[]"],
  ["BasicObject", "instance_exec"], ["Kernel", "instance_exec"],
  ["Module", "class_exec"], ["Module", "module_exec"],
  ["Module", "define_method"],
].freeze

# The helpers live in a module rather than at the top level because a top-level
# `def` is a PRIVATE INSTANCE METHOD OF Object, which the `Object` pass below
# then dumps as though MRI defined it. That contaminated the table with rows for
# this script's own helpers (`esc`, `derived_params`, `encode_params`), and a
# phantom row makes rubylang report the name as defined on every object.
module Gen
  module_function

  def esc(s)
    s.gsub('\\', '\\\\\\\\').gsub('"', '\"')
  end

  # The parameters list MRI reports for a C function of this arity: `n` required
  # for a fixed `n`, and `-(n+1)` means `n` required followed by a rest. Rows whose
  # real parameters differ (Ruby-defined built-ins with names/defaults/keywords)
  # carry them explicitly instead.
  def derived_params(arity)
    arity >= 0 ? [[:req]] * arity : [[:req]] * (-arity - 1) + [[:rest]]
  end

  def encode_params(ps)
    ps.map { |kind, name| name ? "#{kind}:#{name}" : kind.to_s }.join(",")
  end

  # Returned by `sample` when no receiver can be built. Never compare against it
  # with `recv.nil?`: a BasicObject receiver has no `nil?` and raises NoMethodError.
  NORECV = Object.new

  # An instance to probe `cn`'s methods on.
  def sample(cn)
    case cn
    when "BasicObject" then BasicObject.new
    when "Object", "Kernel" then Object.new
    when "Comparable", "Integer", "Numeric" then 1
    when "Enumerable" then [1, 2]
    when "Float" then 1.5
    when "Rational" then Rational(1, 2)
    when "Complex" then Complex(1, 2)
    when "String" then +"abc"
    when "Symbol" then :abc
    when "Array" then [1, 2]
    when "Hash" then { a: 1 }
    when "Range" then (1..3)
    when "NilClass" then nil
    when "TrueClass" then true
    when "FalseClass" then false
    when "Proc" then proc { |_x| 1 }
    when "Method" then 1.method(:to_s)
    when "UnboundMethod" then Integer.instance_method(:to_s)
    when "Module" then Comparable
    when "Class" then String
    when "Regexp" then /a/
    when "MatchData" then /a/.match("a")
    when "Struct" then Struct.new(:a).new(1)
    when "Time" then Time.now
    when "IO", "File" then File.open(File::NULL)
    when "Dir" then Dir.new("/")
    when "Enumerator" then [1, 2].each
    when "Enumerator::Lazy" then [1, 2].lazy
    when "Enumerator::Yielder" then yielder
    when "Thread" then Thread.current
    when "Thread::Mutex" then Thread::Mutex.new
    when "Thread::Queue" then Thread::Queue.new
    when "Thread::SizedQueue" then Thread::SizedQueue.new(1)
    when "Thread::ConditionVariable" then Thread::ConditionVariable.new
    when "Random" then Random.new(1)
    when "Data" then Data.define(:a).new(a: 1)
    when "Set" then Set.new([1])
    when "Binding" then binding
    when "Fiber" then Fiber.new { 1 }
    when "Encoding" then Encoding::UTF_8
    when "Math", "Marshal", "Process", "GC", "ObjectSpace", "Signal", "FileTest", "Warning"
      Object.new.extend(Object.const_get(cn))
    else
      k = Object.const_get(cn)
      (k.is_a?(Class) && k <= Exception) ? k.new("x") : NORECV
    end
  rescue Exception
    NORECV
  end

  # A live Enumerator::Yielder — one only exists while its Enumerator is running.
  def yielder
    grabbed = nil
    Enumerator.new { |y| grabbed = y; y << 1 }.first
    grabbed || NORECV
  rescue Exception
    NORECV
  end

  ARITY_RE = /\Awrong number of arguments \(given \d+, expected (.+)\)\z/
  SENTINEL = Object.new

  # Call `recv.name` with `n` sentinel arguments and report what MRI raised:
  # `[:arity, clause]` for the standard wrong-number wording, `[:other, message]`
  # for any other ArgumentError, or nil when the call was accepted or refused for
  # a reason that is not an ArgumentError.
  def outcome(recv, name, n, block)
    args = Array.new(n) { SENTINEL }
    if block
      recv.__send__(name, *args) { |*| nil }
    else
      recv.__send__(name, *args)
    end
    nil
  rescue ArgumentError => e
    msg = (e.message.dup rescue nil)
    return nil unless msg
    m = ARITY_RE.match(msg)
    return [:arity, m[1]] if m
    # A message that quotes an argument back (`uncaught throw #<probe>`) describes
    # this script's sentinel, not a shape any caller could reproduce.
    msg.include?("#<") ? nil : [:other, msg]
  rescue Exception
    nil
  end

  # A probe far above any real maximum raises before the body runs, so it is the
  # safe way to learn that a method HAS a maximum.
  HIGH = 12

  # The measured `[min, max, clause, zero_arg_message]` for one call shape: `min`
  # -1 when nothing could be measured, `max` -1 when there is no maximum, and
  # `clause` the reference interpreter's own "expected …" wording.
  #
  # The clause is MRI's WORDING, not a measurement, and the two genuinely
  # disagree: `(1..3).min(*12)` reports "expected 1" while `(1..3).min` is
  # perfectly valid. Reading the minimum off the clause therefore makes the guard
  # reject a correct call, so both ends are confirmed by probing — downward from
  # the stated minimum until a count is really refused, and one past the stated
  # maximum to check the maximum exists at all. The clause is still carried
  # verbatim because it is what MRI PRINTS, and the message has to match it.
  def shape(recv, name, block)
    hi = outcome(recv, name, HIGH, block)
    unless hi && hi[0] == :arity
      # It accepted HIGH arguments, so there is no maximum; a zero probe is the
      # only thing that can reveal a minimum.
      lo = outcome(recv, name, 0, block)
      return [-1, -1, nil, nil] unless lo
      return [bounds(lo[1])[0], -1, lo[1], nil] if lo[0] == :arity
      return [-1, -1, nil, lo[1]]
    end

    min, max = bounds(hi[1])
    zero = nil
    while min.positive?
      o = outcome(recv, name, min - 1, block)
      break if o && o[0] == :arity

      zero = o[1] if o && o[0] == :other && min == 1
      min -= 1
    end
    over = outcome(recv, name, max + 1, block)
    max = -1 unless over && over[0] == :arity
    [min, max, hi[1], zero]
  end

  # The keyword this asks about. Chosen so that no built-in can accept it, which
  # is what makes the answer readable.
  KW_PROBE = :__gen_arity_table_probe__

  # Whether the method accepts KEYWORD arguments, measured rather than read off
  # `Method#parameters`. The declared list under-reports exactly the methods that
  # matter here: `Data#with` reports `[[:rest]]` and yet takes nothing BUT
  # keywords, so a table built from the declaration rejects `point.with(x: 9)`.
  #
  # The question only has to be asked of a method with a finite maximum. A method
  # with a rest argument accepts a trailing Hash as a positional under any count,
  # so the caller consuming this column never has to reach for it.
  #
  # An unknown keyword separates the two cases: a method that takes keywords
  # complains about the KEYWORD, and one that does not counts the Hash as one more
  # positional and complains about the COUNT. Both counts are asked because a
  # sentinel is the wrong TYPE for many parameters and a `TypeError` from the
  # positional half answers nothing — `Integer#round` rejects a sentinel before it
  # ever looks at keywords.
  def keywords?(recv, name, min, max)
    return false if max.negative?
    [min, max].uniq.any? { |n| kw_refused?(recv, name, n) }
  end

  # Whether `n` positional arguments plus one unknown keyword is refused FOR THE
  # KEYWORD. False for every other outcome, including the call being accepted:
  # a count MRI accepts needs no leniency from the guard.
  def kw_refused?(recv, name, n)
    recv.__send__(name, *Array.new(n) { SENTINEL }, **{ KW_PROBE => 1 })
    false
  rescue ArgumentError => e
    msg = (e.message rescue nil)
    !msg.nil? && msg.include?("keyword")
  rescue Exception
    false
  end

  # MRI's "expected" clause as a `(min, max)` pair, `max` -1 meaning unbounded.
  def bounds(clause)
    return nil unless clause
    case clause
    when /\A(\d+)\.\.(\d+)\z/ then [$1.to_i, $2.to_i]
    when /\A(\d+)\+\z/ then [$1.to_i, -1]
    when /\A(\d+)\z/ then [$1.to_i, $1.to_i]
    end
  end

  # The clause wording `(min, max)` renders back to. Every measured clause must
  # round-trip through this, which is asserted below — if one ever does not, the
  # pair is not a lossless encoding and the guard would print the wrong message.
  def render(min, max)
    return "#{min}+" if max.negative?
    return min.to_s if min == max
    "#{min}..#{max}"
  end
end

rows = {}
# The class each row was first reached through, and whether it was reached as a
# singleton method — together they supply a receiver to probe the row on.
origin = {}
CLASSES.each do |cn|
  k = begin
    Object.const_get(cn)
  rescue NameError
    warn "gen_arity_table: no such constant #{cn}"
    next
  end
  next unless k.is_a?(Module)
  (k.instance_methods + k.private_instance_methods(false)).each do |m|
    um = begin
      k.instance_method(m)
    rescue NameError, TypeError
      next
    end
    rows[[um.owner.to_s, m.to_s]] ||= [um.arity, um.parameters]
    origin[[um.owner.to_s, m.to_s]] ||= [cn, false]
  end
  k.methods.each do |m|
    mo = begin
      k.method(m)
    rescue NameError
      next
    end
    next unless mo.owner.to_s.start_with?("#<Class:")
    rows[[mo.owner.to_s, m.to_s]] ||= [mo.arity, mo.parameters]
    origin[[mo.owner.to_s, m.to_s]] ||= [cn, true]
  end
end
rows.reject! { |(owner, _), _| owner =~ SKIP_OWNER }


ancestry = {}
CLASSES.each do |cn|
  k = Object.const_get(cn) rescue next
  chain = k.ancestors.map(&:to_s).reject { |a| a =~ SKIP_OWNER }
  ancestry[cn] = chain
end

# Every module name reachable as an owner or in an ancestor chain — a singleton
# lookup skips these (a module has no singleton class in the chain sense).
modules = (CLASSES + ancestry.values.flatten).uniq.select do |cn|
  (Object.const_get(cn) rescue nil).instance_of?(Module)
end

# --- measured argument-count shapes -----------------------------------------
# EVERY reflection read is finished by this point, and this phase must stay last
# for that reason: probing calls real methods, and a called method can change
# what reflection would have reported. Probing `pp` before this block was moved
# autoloaded Ruby's pp library, which injected `PP::ObjectMixin` into the
# ancestor chain of all 31 classes in `BUILTIN_ANCESTRY`.
shapes = {}
zero_errors = {}
problems = []

# A probe that runs a body can PRINT, and this script's standard output is the
# generated file — `Kernel#p` wrote 48 lines of `#<Object:0x…>` into the table
# before this redirect existed. Swap both descriptors for /dev/null across the
# whole probe phase and restore them before a single line is emitted.
saved_out = $stdout.dup
saved_err = $stderr.dup
$stdout.reopen(File::NULL, "w")
$stderr.reopen(File::NULL, "w")
begin
  rows.each_key do |key|
    owner, m = key
    next if UNSAFE.include?(m) || UNSAFE_PAIR.include?([owner, m]) || UNMEASURABLE.include?([owner, m])
    cn, singleton = origin[key]
    recv = singleton ? (Object.const_get(cn) rescue Gen::NORECV) : Gen.sample(cn)
    next if Gen::NORECV.equal?(recv)

    nmin, nmax, nclause, nz = Gen.shape(recv, m, false)
    bmin, bmax, bclause, bz = Gen.shape(recv, m, true)
    [nclause, bclause].compact.each do |c|
      problems << "unparsable clause #{c.inspect} for #{owner}##{m}" unless Gen.bounds(c)
    end
    # A row that constrains nothing needs no entry — the guard checks only what
    # it can find, so leaving it out is what "unmeasured" means.
    unless nmin.negative? && nmax.negative? && bmin.negative? && bmax.negative?
      # The declared parameter list and the probe are UNIONED. Each catches
      # methods the other misses — the declaration alone misses `Data#with`, the
      # probe alone misses a method whose positional parameters reject a sentinel
      # at both ends — and the column only ever widens what is accepted, so a
      # false positive costs a missed error while a false negative costs a
      # wrongly-raised one.
      declared = rows[key][1].any? { |kind,| %i[key keyreq keyrest].include?(kind) }
      kw = declared || Gen.keywords?(recv, m, nmin, nmax)
      shapes[key] = [nmin, nmax, nclause || "", bmin, bmax, bclause || "", kw]
    end
    zero_errors[key] = [nz, bz] if nz || bz
  end
ensure
  $stdout.reopen(saved_out)
  $stderr.reopen(saved_err)
end
abort "gen_arity_table: #{problems.join("; ")}" unless problems.empty?

puts <<~HDR
  //! MRI's declared built-in method shapes — the arity, defining module and
  //! parameter list of every method the reference interpreter defines on the core
  //! classes.
  //!
  //! A built-in is native code with no written parameter list, so `Method#arity`,
  //! `#owner` and `#parameters` cannot be derived from it the way a `def` allows;
  //! without this table they can only report "variadic, owner unknown". Every row
  //! is what the reference `ruby` reports for that method.
  //!
  //! GENERATED — do not edit by hand. Regenerate against the reference
  //! interpreter with:
  //!
  //! ```text
  //! ruby tools/gen_arity_table.rb > src/arity_table.rs && cargo fmt
  //! ```

  /// `(owner, method, arity, params)`, sorted by `(owner, method)` for binary
  /// search. An empty `params` means the list is the one implied by the arity
  /// (see [`params_for`]); a non-empty one is `kind[:name]`, comma separated.
  pub static BUILTIN_METHODS: &[(&str, &str, i16, &str)] = &[
HDR

rows.sort_by { |(owner, m), _| [owner, m] }.each do |(owner, m), (arity, ps)|
  enc = ps == Gen.derived_params(arity) ? "" : Gen.encode_params(ps)
  puts %{    ("#{Gen.esc(owner)}", "#{Gen.esc(m)}", #{arity}, "#{Gen.esc(enc)}"),}
end

puts <<~MID
  ];

  /// The ancestor chain the reference interpreter reports for each built-in
  /// class — the order `#owner` resolution walks. Sorted by class name.
  pub static BUILTIN_ANCESTRY: &[(&str, &[&str])] = &[
MID

ancestry.sort.each do |cn, chain|
  puts %{    ("#{Gen.esc(cn)}", &[#{chain.map { |c| %{"#{Gen.esc(c)}"} }.join(", ")}]),}
end

puts <<~MID2
  ];

  /// The built-in names that are modules, not classes — they have no `new`, and
  /// their singleton chain skips `Class`. Sorted.
  pub static BUILTIN_MODULES: &[&str] = &[
MID2

modules.sort.each { |m| puts %{    "#{Gen.esc(m)}",} }

puts <<~MID3
  ];

  /// How many arguments each built-in actually accepts, MEASURED by calling the
  /// reference interpreter with a wrong count and reading the range out of the
  /// `ArgumentError` it raises. `Method#arity` cannot express this — `String#center`
  /// reports -1 and takes 1..2 — so nothing here is derived from the arity column.
  ///
  /// `(owner, method, min, max, expected, block_min, block_max, block_expected,
  /// keywords)`, sorted for binary search. A `min` of -1 means the shape was not
  /// measured and nothing is checked; a `max` of -1 means the method takes a rest
  /// argument and has no maximum. The `block_*` triple is the shape when a block
  /// is passed, which for some methods is wider (`String#sub` takes 2 without a
  /// block and 1..2 with one). `keywords` is true when the method accepts keyword
  /// arguments, which lets a caller allow the one trailing Hash rubylang
  /// represents keyword arguments as. That column is measured too: `Data#with`
  /// declares `[[:rest]]` and accepts nothing but keywords, so reading it off the
  /// declared parameter list would reject `point.with(x: 9)`.
  ///
  /// `expected` is what MRI PRINTS inside `wrong number of arguments (given N,
  /// expected …)`, carried verbatim rather than rebuilt from `min`/`max` because
  /// the two do not always agree — `Range#min` accepts 0..1 but prints
  /// "expected 1". The bounds decide whether to raise; this string is the message.
  pub type ArgShapeRow = (
      &'static str,
      &'static str,
      i16,
      i16,
      &'static str,
      i16,
      i16,
      &'static str,
      bool,
  );

  /// The measured argument-count shapes, one `ArgShapeRow` per built-in.
  pub static BUILTIN_ARG_SHAPES: &[ArgShapeRow] = &[
MID3

shapes.sort_by { |(owner, m), _| [owner, m] }.each do |(owner, m), sh|
  nmin, nmax, nclause, bmin, bmax, bclause, kw = sh
  cols = [
    %("#{Gen.esc(owner)}"), %("#{Gen.esc(m)}"),
    nmin, nmax, %("#{Gen.esc(nclause)}"),
    bmin, bmax, %("#{Gen.esc(bclause)}"), kw
  ]
  puts "    (#{cols.join(', ')}),"
end

puts <<~MID4
  ];

  /// The built-ins that answer a zero-argument call with an `ArgumentError` whose
  /// message is NOT the standard wrong-number wording — `1.send()` says "no method
  /// name given". Measured the same way, and stored verbatim.
  ///
  /// `(owner, method, message, message_with_block)`; an empty string means the call
  /// raises nothing in that form.
  pub static BUILTIN_ZERO_ARG_ERRORS: &[(&str, &str, &str, &str)] = &[
MID4

zero_errors.sort_by { |(owner, m), _| [owner, m] }.each do |(owner, m), (nz, bz)|
  puts %{    ("#{Gen.esc(owner)}", "#{Gen.esc(m)}", "#{Gen.esc(nz || "")}", "#{Gen.esc(bz || "")}"),}
end

puts <<~'TAIL'
];

/// The row `(owner, arity, params)` the reference interpreter declares for
/// `method` on `owner`, or `None` when `owner` does not define it. The owner is
/// returned as the table's own `'static` string so callers can keep it.
pub fn lookup(owner: &str, method: &str) -> Option<(&'static str, i16, &'static str)> {
    BUILTIN_METHODS
        .binary_search_by(|(o, m, _, _)| (*o, *m).cmp(&(owner, method)))
        .ok()
        .map(|i| (BUILTIN_METHODS[i].0, BUILTIN_METHODS[i].2, BUILTIN_METHODS[i].3))
}

/// The reference ancestor chain of built-in class `name`, or `None` when it is
/// not a built-in.
pub fn ancestry(name: &str) -> Option<&'static [&'static str]> {
    BUILTIN_ANCESTRY
        .binary_search_by(|(c, _)| c.cmp(&name))
        .ok()
        .map(|i| BUILTIN_ANCESTRY[i].1)
}

/// Whether `name` is a built-in module (as opposed to a class).
pub fn is_module(name: &str) -> bool {
    BUILTIN_MODULES.binary_search(&name).is_ok()
}

/// The measured argument-count shape `owner` declares for `method`: the accepted
/// `(min, max)` and printed clause without a block, the same three with one, and
/// whether the reference interpreter declares keyword parameters. A `min` of -1
/// means unmeasured, a `max` of -1 means unbounded.
pub type ArgShape = (i16, i16, &'static str, i16, i16, &'static str, bool);

/// The row of [`BUILTIN_ARG_SHAPES`] for `owner`'s `method`.
pub fn arg_shape(owner: &str, method: &str) -> Option<ArgShape> {
    BUILTIN_ARG_SHAPES
        .binary_search_by(|(o, m, ..)| (*o, *m).cmp(&(owner, method)))
        .ok()
        .map(|i| {
            let r = &BUILTIN_ARG_SHAPES[i];
            (r.2, r.3, r.4, r.5, r.6, r.7, r.8)
        })
}

/// The verbatim message the reference interpreter raises for a zero-argument call
/// to `owner`'s `method` when it is not the standard wrong-number wording, for the
/// block-less and block forms. An empty string means that form raises nothing.
pub fn zero_arg_error(owner: &str, method: &str) -> Option<(&'static str, &'static str)> {
    BUILTIN_ZERO_ARG_ERRORS
        .binary_search_by(|(o, m, _, _)| (*o, *m).cmp(&(owner, method)))
        .ok()
        .map(|i| (BUILTIN_ZERO_ARG_ERRORS[i].2, BUILTIN_ZERO_ARG_ERRORS[i].3))
}


/// `Method#parameters` for a table row: the explicit list when the row carries
/// one, otherwise the list MRI reports for a C function of that arity — `n`
/// unnamed required parameters, and for a negative arity `-(n+1)` those `n`
/// followed by an unnamed rest.
pub fn params_for(arity: i16, params: &'static str) -> Vec<(&'static str, Option<String>)> {
    if !params.is_empty() {
        return params
            .split(',')
            .map(|tok| match tok.split_once(':') {
                Some((kind, name)) => (param_kind(kind), Some(name.to_string())),
                None => (param_kind(tok), None),
            })
            .collect();
    }
    let req = if arity >= 0 { arity } else { -arity - 1 };
    let mut out: Vec<(&'static str, Option<String>)> = (0..req).map(|_| ("req", None)).collect();
    if arity < 0 {
        out.push(("rest", None));
    }
    out
}

/// Intern a parameter-kind token from the table back to a `'static` name.
fn param_kind(k: &str) -> &'static str {
    match k {
        "req" => "req",
        "opt" => "opt",
        "rest" => "rest",
        "keyreq" => "keyreq",
        "key" => "key",
        "keyrest" => "keyrest",
        "block" => "block",
        "nokey" => "nokey",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}
TAIL
