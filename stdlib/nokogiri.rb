# A minimal pure-Ruby Nokogiri compatibility skeleton, bundled into rubylang.
#
# The real Nokogiri is a C extension (its `require "nokogiri"` loads the native
# `nokogiri/nokogiri` shared object), which cannot load in this runtime. Rails
# pulls Nokogiri in *eagerly* through rails-html-sanitizer → Loofah when
# ActionView's SanitizeHelper module is defined — so the require fires during
# view setup even for responses that contain no HTML (`render plain:`, JSON,
# redirects). This shim provides only the class skeleton and node-type constants
# those gems reference at *load* time so they load cleanly.
#
# Actual HTML/XML parsing and scrubbing are NOT implemented: any method that
# would parse or serialize a document raises NotImplementedError. Responses that
# never invoke the sanitizer (plain text, JSON, redirects, and templates that
# don't call `sanitize`) work; a response that actually sanitizes HTML needs the
# real native Nokogiri.
module Nokogiri
  VERSION = "1.19.4"

  # Loofah gates its HTML5 code path on `uses_gumbo?`; returning false keeps
  # `Loofah.html5_support?` false so the html5/* files (which lean harder on the
  # native parser) are never required.
  def self.uses_gumbo?
    false
  end

  def self.parse(*)
    raise NotImplementedError, "Nokogiri parsing is unavailable (native extension not loaded)"
  end

  module XML
    # Libxml2 node-type constants (loofah's scrubbers branch on these).
    class Node
      ELEMENT_NODE       = 1
      ATTRIBUTE_NODE     = 2
      TEXT_NODE          = 3
      CDATA_SECTION_NODE = 4
      ENTITY_REF_NODE    = 5
      ENTITY_NODE        = 6
      PI_NODE            = 7
      COMMENT_NODE       = 8
      DOCUMENT_NODE      = 9
      DOCUMENT_TYPE_NODE = 10
      DOCUMENT_FRAG_NODE = 11
      NOTATION_NODE      = 12
      HTML_DOCUMENT_NODE = 13
      DTD_NODE           = 14
      ELEMENT_DECL       = 15
      ATTRIBUTE_DECL     = 16
      ENTITY_DECL        = 17
      NAMESPACE_DECL     = 18
      XINCLUDE_START     = 19
      XINCLUDE_END       = 20
    end

    class CharacterData < Node; end

    class Text < CharacterData
      def initialize(_content, _document = nil); end
    end

    class NodeSet
      include Enumerable
      def each; end
    end

    class Document < Node
      # Per-class decorator module lists (Loofah's DocumentDecorator#initialize
      # appends its scrub behaviors here when a document is instantiated).
      def decorators(klass)
        (@decorators ||= {})[klass] ||= []
      end
    end

    class DocumentFragment < Node
      def decorators(klass)
        (@decorators ||= {})[klass] ||= []
      end
    end
  end

  module HTML4
    class Document < Nokogiri::XML::Document; end
    class DocumentFragment < Nokogiri::XML::DocumentFragment; end
  end

  # `Nokogiri::HTML` is the legacy alias for the HTML4 namespace.
  HTML = HTML4
end
