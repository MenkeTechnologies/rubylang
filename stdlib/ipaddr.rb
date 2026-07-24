# ipaddr — a pragmatic port of Ruby's stdlib IPAddr, bundled into rubylang and
# loaded by `require "ipaddr"`. Parses IPv4/IPv6 addresses and CIDR networks and
# tests membership (`IPAddr.new("10.0.0.0/8").include?("10.1.2.3")`). rack, rails,
# and rack-protection use it for host/IP allow-lists.

class IPAddr
  class InvalidAddressError < ArgumentError; end
  class AddressFamilyError < ArgumentError; end

  IN4MASK = 0xffffffff
  IN6MASK = 0xffffffffffffffffffffffffffffffff

  attr_reader :family

  # `IPAddr.new("1.2.3.4")` / `IPAddr.new("10.0.0.0/8")` / `IPAddr.new("::1")` /
  # `IPAddr.new("::/0")`. Stores the network as an integer address plus a mask.
  def initialize(addr = "::", family = nil)
    if addr.is_a?(Integer)
      @addr = addr
      @family = family || (addr > IN4MASK ? :inet6 : :inet)
      @mask_addr = @family == :inet6 ? IN6MASK : IN4MASK
      @prefix = @family == :inet6 ? 128 : 32
      return
    end

    ip, prefix = addr.to_s.split("/", 2)
    if ip.include?(":")
      @family = :inet6
      full = 128
      @mask_addr = IN6MASK
      @addr = parse_ipv6(ip)
    else
      @family = :inet
      full = 32
      @mask_addr = IN4MASK
      @addr = parse_ipv4(ip)
    end

    @prefix = prefix ? prefix.to_i : full
    @mask_addr = mask_for(@prefix, full)
    @addr &= @mask_addr
  end

  # Whether `other` (an IPAddr, an address string, or an Integer) lies within
  # this network.
  def include?(other)
    other = self.class.new(other) unless other.is_a?(IPAddr)
    return false unless other.family == @family
    (other.to_i & @mask_addr) == @addr
  end
  alias === include?
  alias cover? include?

  def to_i
    @addr
  end

  def to_range
    self.class.new(@addr, @family)..self.class.new(@addr | ~@mask_addr & full_mask, @family)
  end

  def ipv4?
    @family == :inet
  end

  def ipv6?
    @family == :inet6
  end

  def to_s
    ipv4? ? int_to_ipv4(@addr) : int_to_ipv6(@addr)
  end

  def inspect
    "#<IPAddr: #{@family.to_s.sub('inet6', 'IPv6').sub('inet', 'IPv4')}:#{self}/#{int_to_mask}>"
  end

  def ==(other)
    other.is_a?(IPAddr) && @addr == other.to_i && @family == other.family
  end

  def hash
    [@addr, @family].hash
  end

  private

  def full_mask
    ipv4? ? IN4MASK : IN6MASK
  end

  def mask_for(prefix, full)
    return 0 if prefix <= 0
    prefix = full if prefix > full
    (full_mask >> (full - prefix)) << (full - prefix)
  end

  def parse_ipv4(str)
    parts = str.split(".")
    raise InvalidAddressError, "invalid address: #{str}" unless parts.size == 4
    parts.reduce(0) do |acc, p|
      n = p.to_i
      raise InvalidAddressError, "invalid address: #{str}" unless n.between?(0, 255)
      (acc << 8) | n
    end
  end

  # A pragmatic IPv6 parser: handles a single `::` compression and hex groups.
  def parse_ipv6(str)
    if str == "::"
      return 0
    end
    head, tail = str.split("::", 2)
    head_groups = head.to_s.empty? ? [] : head.split(":")
    tail_groups = tail.to_s.empty? ? [] : tail.split(":")
    fill = 8 - head_groups.size - tail_groups.size
    groups = head_groups + (tail ? Array.new([fill, 0].max, "0") : []) + tail_groups
    raise InvalidAddressError, "invalid address: #{str}" if groups.size != 8
    groups.reduce(0) { |acc, g| (acc << 16) | g.to_i(16) }
  end

  def int_to_ipv4(n)
    [(n >> 24) & 0xff, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff].join(".")
  end

  def int_to_ipv6(n)
    (0..7).map { |i| ((n >> (16 * (7 - i))) & 0xffff).to_s(16) }.join(":")
  end

  def int_to_mask
    m = @mask_addr
    count = 0
    bit = full_mask
    while (m & (bit >> count)) != 0 && count < (ipv4? ? 32 : 128)
      count += 1
    end
    count
  end
end
