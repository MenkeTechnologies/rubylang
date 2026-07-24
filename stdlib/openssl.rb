# openssl — a pragmatic pure-Ruby subset of Ruby's OpenSSL binding, bundled into
# rubylang and loaded by `require "openssl"`. Ruby's real `openssl.rb` shim
# `require`s a C extension (`openssl.so`) that rubylang cannot load, so `OpenSSL`
# would never be defined and any `OpenSSL::…` reference (activesupport's
# MessageEncryptor, KeyGenerator, cookie signing) raises. This bundle supplies
# the module/class/constant surface Rails touches at load time and hex-digest
# hashing backed by rubylang's native Digest.
#
# LIMITATION: rubylang has no ASCII-8BIT (binary) string encoding yet — every
# string is UTF-8, so bytes ≥ 128 widen to two bytes. Raw-binary crypto (cipher
# encrypt/decrypt, byte-exact HMAC/PBKDF2) therefore cannot be byte-correct.
# hexdigest hashing is correct; HMAC/KDF are deterministic best-effort so an app
# boots and serves; symmetric encryption raises NotImplementedError.

module OpenSSL
  VERSION = "3.0.0".freeze
  OPENSSL_VERSION = "rubylang-openssl-shim".freeze

  class OpenSSLError < StandardError; end

  # Message digests. `OpenSSL::Digest::SHA256`, `::SHA1`, `::MD5` and the generic
  # `OpenSSL::Digest.new("SHA256")` all funnel to rubylang's native `Digest`.
  class Digest
    # native Digest class per OpenSSL digest name (only what Rails 8 uses).
    NATIVE = {
      "MD5" => ::Digest::MD5,
      "SHA1" => ::Digest::SHA1,
      "SHA256" => ::Digest::SHA256,
    }.freeze

    def self.digest(name, data)
      lookup(name).digest(data)
    end

    def self.hexdigest(name, data)
      lookup(name).hexdigest(data)
    end

    def self.lookup(name)
      NATIVE[name.to_s.upcase] ||
        raise(OpenSSLError, "unsupported digest algorithm (#{name})")
    end

    def initialize(name)
      @native = self.class.lookup(name)
      @buf = +""
    end

    def update(data)
      @buf << data.to_s
      self
    end
    alias << update

    def digest
      @native.digest(@buf)
    end

    def hexdigest
      @native.hexdigest(@buf)
    end

    def reset
      @buf = +""
      self
    end

    # Concrete named subclasses: OpenSSL::Digest::SHA256.new / .hexdigest(str).
    class SHA256 < Digest
      def initialize
        super("SHA256")
      end

      def self.digest(data)
        ::Digest::SHA256.digest(data)
      end

      def self.hexdigest(data)
        ::Digest::SHA256.hexdigest(data)
      end
    end

    class SHA1 < Digest
      def initialize
        super("SHA1")
      end

      def self.digest(data)
        ::Digest::SHA1.digest(data)
      end

      def self.hexdigest(data)
        ::Digest::SHA1.hexdigest(data)
      end
    end

    class MD5 < Digest
      def initialize
        super("MD5")
      end

      def self.digest(data)
        ::Digest::MD5.digest(data)
      end

      def self.hexdigest(data)
        ::Digest::MD5.hexdigest(data)
      end
    end
  end

  # Keyed-hash MAC. Standard HMAC construction over the native digest. Byte-exact
  # only for ASCII keys/data (see the binary-encoding limitation above); always
  # deterministic, which is what cookie/verifier boot paths require.
  module HMAC
    BLOCK = 64 # block size in bytes for MD5/SHA1/SHA256

    def self.digest(digest, key, data)
      raw = compute(digest, key, data)
      [raw].pack("H*")
    end

    def self.hexdigest(digest, key, data)
      compute(digest, key, data)
    end

    # Returns the HMAC as a hex string. Operates on byte arrays so no binary
    # string has to survive a round-trip.
    def self.compute(digest, key, data)
      klass = digest.is_a?(String) ? Digest.lookup(digest) : digest_class(digest)
      key_bytes = key.to_s.bytes
      key_bytes = hex_to_bytes(klass.hexdigest(key.to_s)) if key_bytes.length > BLOCK
      key_bytes += [0] * (BLOCK - key_bytes.length)
      ipad = key_bytes.map { |b| b ^ 0x36 }
      opad = key_bytes.map { |b| b ^ 0x5c }
      inner = klass.hexdigest(bytes_to_str(ipad) + data.to_s)
      klass.hexdigest(bytes_to_str(opad) + bytes_to_str(hex_to_bytes(inner)))
    end

    def self.digest_class(d)
      return d if d.respond_to?(:hexdigest)
      Digest.lookup(d.to_s)
    end

    def self.hex_to_bytes(hex)
      hex.scan(/../).map { |h| h.to_i(16) }
    end

    def self.bytes_to_str(bytes)
      bytes.map(&:chr).join
    end
  end

  # Key-derivation. Best-effort PBKDF2 built on HMAC — deterministic, sufficient
  # to derive the per-app keys Rails generates at boot.
  module KDF
    def self.pbkdf2_hmac(pass, salt:, iterations:, length:, hash:)
      klass = OpenSSL::Digest.lookup(hash.to_s)
      block = OpenSSL::HMAC.hex_to_bytes(OpenSSL::HMAC.compute(klass, pass, "#{salt}\x00\x00\x00\x01"))
      out = block.dup
      (iterations - 1).times do
        block = OpenSSL::HMAC.hex_to_bytes(
          OpenSSL::HMAC.compute(klass, pass, OpenSSL::HMAC.bytes_to_str(block))
        )
        block.each_index { |i| out[i] ^= block[i] }
      end
      OpenSSL::HMAC.bytes_to_str(out[0, length])
    end
  end

  # The legacy PBKDF2 entry points (activesupport KeyGenerator uses these).
  module PKCS5
    def self.pbkdf2_hmac_sha1(pass, salt, iterations, length)
      OpenSSL::KDF.pbkdf2_hmac(pass, salt: salt, iterations: iterations, length: length, hash: "SHA1")
    end

    def self.pbkdf2_hmac(pass, salt, iterations, length, digest)
      name = digest.respond_to?(:name) ? digest.name : digest.to_s
      OpenSSL::KDF.pbkdf2_hmac(pass, salt: salt, iterations: iterations, length: length, hash: name)
    end
  end

  # Symmetric ciphers. The class/constant surface exists so Rails' MessageEncryptor
  # loads; actual encryption needs binary strings rubylang doesn't have yet.
  class Cipher
    class CipherError < OpenSSLError; end

    def self.ciphers
      []
    end

    def initialize(name)
      @name = name.to_s
    end

    attr_reader :name

    def key_len
      # AES-256 uses a 32-byte key; anything with "128" a 16-byte one.
      @name.include?("128") ? 16 : 32
    end

    def iv_len
      16
    end

    def encrypt
      unsupported!
    end

    def decrypt
      unsupported!
    end

    def key=(_)
      unsupported!
    end

    def iv=(_)
      unsupported!
    end

    def update(_)
      unsupported!
    end

    def final
      unsupported!
    end

    def unsupported!
      raise NotImplementedError,
            "OpenSSL::Cipher needs binary (ASCII-8BIT) strings, unsupported in rubylang"
    end
  end

  # Random bytes, delegated to the native SecureRandom.
  module Random
    def self.random_bytes(n)
      SecureRandom.bytes(n)
    end
  end
end
