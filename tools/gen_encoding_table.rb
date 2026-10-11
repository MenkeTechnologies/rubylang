# Regenerates `src/encoding_table.rs` — every encoding the reference
# interpreter defines: its name, its alias names, whether it is a dummy, whether
# it is ASCII-compatible, whether it is loaded at boot (MRI inspects an
# encoding that has not been loaded yet as `#<Encoding:EUC-JP (autoload)>`),
# and the `Encoding::` constants that name it.
#
#   /opt/homebrew/opt/ruby/bin/ruby tools/gen_encoding_table.rb > src/encoding_table.rs \
#     && cargo fmt
#
# MUST be a real MRI, given by absolute path. Bare `ruby` is rubylang itself
# wherever rubylang is installed, and regenerating this table from rubylang
# would freeze rubylang's own answers as the reference it is measured against.
#
# The locale-dependent pseudo-names (`locale`, `external`, `filesystem`,
# `internal`) are left out of the alias lists: they name whichever encoding the
# process defaults to, which rubylang resolves at runtime.

abort "gen_encoding_table.rb: RUBY_ENGINE is #{RUBY_ENGINE.inspect}, not \"ruby\"" unless RUBY_ENGINE == "ruby"

SPECIAL = %w[locale external filesystem internal].freeze

# Read the boot-loaded state FIRST: `names`, `dummy?` and `ascii_compatible?`
# each load the encoding they are asked about.
encs = Encoding.list
preloaded = encs.to_h { |e| [e, !e.inspect.include?("(autoload)")] }

consts = Hash.new { |h, k| h[k] = [] }
Encoding.constants.sort.each do |c|
  v = Encoding.const_get(c)
  consts[v] << c.to_s if v.is_a?(Encoding)
end

q = ->(s) { s.inspect }

puts <<~HEAD
  //! Every encoding MRI defines — name, aliases, dummy / ASCII-compatible
  //! flags, boot-loaded state and `Encoding::` constant names — in
  //! `Encoding.list` order.
  //!
  //! GENERATED — do not edit by hand. Regenerate against the reference
  //! interpreter, named by ABSOLUTE PATH, with:
  //!
  //! ```text
  //! /opt/homebrew/opt/ruby/bin/ruby tools/gen_encoding_table.rb > src/encoding_table.rs
  //! cargo fmt
  //! ```
  //!
  //! Generated from #{RUBY_DESCRIPTION}.

  /// One encoding's row.
  pub struct EncodingInfo {
      /// The canonical name (`Encoding#name`).
      pub name: &'static str,
      /// `Encoding#names` without the locale-dependent pseudo-names.
      pub names: &'static [&'static str],
      pub dummy: bool,
      pub ascii_compatible: bool,
      /// Loaded at boot; any other encoding inspects with ` (autoload)` until
      /// something loads it.
      pub preloaded: bool,
      /// The `Encoding::` constants bound to it.
      pub constants: &'static [&'static str],
  }

  pub static ENCODINGS: &[EncodingInfo] = &[
HEAD
encs.each do |e|
  names = e.names - SPECIAL
  puts "    EncodingInfo {"
  puts "        name: #{q[e.name]},"
  puts "        names: &[#{names.map(&q).join(", ")}],"
  puts "        dummy: #{e.dummy?},"
  puts "        ascii_compatible: #{e.ascii_compatible?},"
  puts "        preloaded: #{preloaded[e]},"
  puts "        constants: &[#{consts[e].map(&q).join(", ")}],"
  puts "    },"
end
puts "];"
puts
puts "/// `Encoding.name_list` without the pseudo-names, which MRI lists last."
puts "pub static NAME_LIST: &[&str] = &[#{(Encoding.name_list - SPECIAL).map(&q).join(", ")}];"
puts
puts "/// `Encoding.aliases` as `(alias, name)` pairs without the pseudo-names, which"
puts "/// MRI lists last."
puts "pub static ALIASES: &[(&str, &str)] = &["
Encoding.aliases.each { |a, n| puts "    (#{q[a]}, #{q[n]})," unless SPECIAL.include?(a) }
puts "];"
