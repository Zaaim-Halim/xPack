package io.xpack.internal;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The little JSON this plugin needs, written out and read back.
 *
 * <p>Deliberately not a library. A packaging plugin sits on the build
 * classpath of every project that uses it, so a JSON dependency here becomes a
 * version conflict in somebody else's build. The manifest is a handful of
 * fields and the command line's output is a flat object, so the cost of owning
 * this is far lower than the cost of imposing a dependency.
 */
public final class Json {

    private Json() {
    }

    // ---------------------------------------------------------------- writing

    /** Builds a JSON object, preserving insertion order so output is stable. */
    public static final class Obj {
        private final Map<String, Object> fields = new LinkedHashMap<>();

        /** Adds a field, ignoring it when the value is null. */
        public Obj put(String key, Object value) {
            if (value != null) {
                fields.put(key, value);
            }
            return this;
        }

        /** Adds an object field, ignoring it when it has no fields of its own. */
        public Obj putIfAny(String key, Obj value) {
            if (value != null && !value.fields.isEmpty()) {
                fields.put(key, value);
            }
            return this;
        }

        /** Adds a list field, ignoring it when the list is null or empty. */
        public Obj putIfAny(String key, List<?> value) {
            if (value != null && !value.isEmpty()) {
                fields.put(key, value);
            }
            return this;
        }

        /** Adds a map field, ignoring it when the map is null or empty. */
        public Obj putIfAny(String key, Map<String, String> value) {
            if (value != null && !value.isEmpty()) {
                fields.put(key, new LinkedHashMap<String, Object>(value));
            }
            return this;
        }

        public boolean isEmpty() {
            return fields.isEmpty();
        }

        @Override
        public String toString() {
            StringBuilder out = new StringBuilder();
            write(fields, out, 0);
            out.append('\n');
            return out.toString();
        }
    }

    private static void write(Object value, StringBuilder out, int depth) {
        if (value instanceof Obj obj) {
            write(obj.fields, out, depth);
        } else if (value instanceof Map<?, ?> map) {
            if (map.isEmpty()) {
                out.append("{}");
                return;
            }
            out.append("{\n");
            int i = 0;
            for (Map.Entry<?, ?> e : map.entrySet()) {
                indent(out, depth + 1);
                quote(String.valueOf(e.getKey()), out);
                out.append(": ");
                write(e.getValue(), out, depth + 1);
                if (++i < map.size()) {
                    out.append(',');
                }
                out.append('\n');
            }
            indent(out, depth);
            out.append('}');
        } else if (value instanceof List<?> list) {
            if (list.isEmpty()) {
                out.append("[]");
                return;
            }
            out.append('[');
            for (int i = 0; i < list.size(); i++) {
                if (i > 0) {
                    out.append(", ");
                }
                write(list.get(i), out, depth);
            }
            out.append(']');
        } else if (value instanceof String s) {
            quote(s, out);
        } else if (value instanceof Boolean || value instanceof Number) {
            out.append(value);
        } else if (value == null) {
            out.append("null");
        } else {
            quote(String.valueOf(value), out);
        }
    }

    private static void indent(StringBuilder out, int depth) {
        out.append("  ".repeat(depth));
    }

    private static void quote(String s, StringBuilder out) {
        out.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                case '\b' -> out.append("\\b");
                case '\f' -> out.append("\\f");
                default -> {
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
                }
            }
        }
        out.append('"');
    }

    // ---------------------------------------------------------------- reading

    /**
     * Parses a JSON document into {@code Map}, {@code List}, {@code String},
     * {@code Double}, {@code Long}, {@code Boolean} and null.
     *
     * @throws IllegalArgumentException when the text is not valid JSON
     */
    public static Object parse(String text) {
        Parser parser = new Parser(text);
        parser.skipWhitespace();
        Object value = parser.value();
        parser.skipWhitespace();
        if (!parser.atEnd()) {
            throw new IllegalArgumentException("trailing content at offset " + parser.position());
        }
        return value;
    }

    /** Parses a document expected to be an object. */
    @SuppressWarnings("unchecked")
    public static Map<String, Object> parseObject(String text) {
        Object value = parse(text);
        if (!(value instanceof Map)) {
            throw new IllegalArgumentException("expected a JSON object");
        }
        return (Map<String, Object>) value;
    }

    /** Reads a required string field. */
    public static String string(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (!(value instanceof String s)) {
            throw new IllegalArgumentException("missing string field " + key);
        }
        return s;
    }

    /**
     * Reads a required nested object.
     *
     * <p>The command line is not uniform about this: {@code pack} reports a
     * platform as the string {@code "macos-arm64"} while {@code inspect}
     * reports the manifest's own {@code {os, arch}}. Assuming either shape
     * everywhere is how three goals came to read a field that was not there.
     */
    @SuppressWarnings("unchecked")
    public static Map<String, Object> object(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (!(value instanceof Map)) {
            throw new IllegalArgumentException("missing object field " + key);
        }
        return (Map<String, Object>) value;
    }

    /** Reads a platform, whichever of the two shapes the caller was given. */
    public static String platform(Map<String, Object> object) {
        Object value = object.get("platform");
        if (value instanceof String text) {
            return text;
        }
        Map<String, Object> nested = object(object, "platform");
        return string(nested, "os") + "-" + string(nested, "arch");
    }

    /** Reads a required integral field. */
    public static long number(Map<String, Object> object, String key) {
        Object value = object.get(key);
        if (!(value instanceof Number n)) {
            throw new IllegalArgumentException("missing numeric field " + key);
        }
        return n.longValue();
    }

    private static final class Parser {
        private final String text;
        private int at;

        Parser(String text) {
            this.text = text;
        }

        int position() {
            return at;
        }

        boolean atEnd() {
            return at >= text.length();
        }

        void skipWhitespace() {
            while (at < text.length() && Character.isWhitespace(text.charAt(at))) {
                at++;
            }
        }

        Object value() {
            if (atEnd()) {
                throw new IllegalArgumentException("unexpected end of input");
            }
            return switch (text.charAt(at)) {
                case '{' -> object();
                case '[' -> array();
                case '"' -> string();
                case 't' -> literal("true", Boolean.TRUE);
                case 'f' -> literal("false", Boolean.FALSE);
                case 'n' -> literal("null", null);
                default -> number();
            };
        }

        private Map<String, Object> object() {
            Map<String, Object> result = new LinkedHashMap<>();
            expect('{');
            skipWhitespace();
            if (peek() == '}') {
                at++;
                return result;
            }
            while (true) {
                skipWhitespace();
                String key = string();
                skipWhitespace();
                expect(':');
                skipWhitespace();
                result.put(key, value());
                skipWhitespace();
                char c = next();
                if (c == '}') {
                    return result;
                }
                if (c != ',') {
                    throw new IllegalArgumentException("expected , or } at offset " + (at - 1));
                }
            }
        }

        private List<Object> array() {
            List<Object> result = new ArrayList<>();
            expect('[');
            skipWhitespace();
            if (peek() == ']') {
                at++;
                return result;
            }
            while (true) {
                skipWhitespace();
                result.add(value());
                skipWhitespace();
                char c = next();
                if (c == ']') {
                    return result;
                }
                if (c != ',') {
                    throw new IllegalArgumentException("expected , or ] at offset " + (at - 1));
                }
            }
        }

        private String string() {
            expect('"');
            StringBuilder out = new StringBuilder();
            while (true) {
                char c = next();
                if (c == '"') {
                    return out.toString();
                }
                if (c != '\\') {
                    out.append(c);
                    continue;
                }
                char escape = next();
                switch (escape) {
                    case '"' -> out.append('"');
                    case '\\' -> out.append('\\');
                    case '/' -> out.append('/');
                    case 'b' -> out.append('\b');
                    case 'f' -> out.append('\f');
                    case 'n' -> out.append('\n');
                    case 'r' -> out.append('\r');
                    case 't' -> out.append('\t');
                    case 'u' -> {
                        if (at + 4 > text.length()) {
                            throw new IllegalArgumentException("truncated \\u escape");
                        }
                        out.append((char) Integer.parseInt(text.substring(at, at + 4), 16));
                        at += 4;
                    }
                    default -> throw new IllegalArgumentException(
                            "unknown escape \\" + escape + " at offset " + (at - 1));
                }
            }
        }

        private Object number() {
            int start = at;
            if (peek() == '-') {
                at++;
            }
            boolean fractional = false;
            while (!atEnd()) {
                char c = text.charAt(at);
                if (c >= '0' && c <= '9') {
                    at++;
                } else if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
                    fractional = true;
                    at++;
                } else {
                    break;
                }
            }
            String token = text.substring(start, at);
            if (token.isEmpty() || token.equals("-")) {
                throw new IllegalArgumentException("expected a value at offset " + start);
            }
            return fractional ? (Object) Double.parseDouble(token) : (Object) Long.parseLong(token);
        }

        private Object literal(String word, Object value) {
            if (!text.startsWith(word, at)) {
                throw new IllegalArgumentException("expected " + word + " at offset " + at);
            }
            at += word.length();
            return value;
        }

        private char peek() {
            if (atEnd()) {
                throw new IllegalArgumentException("unexpected end of input");
            }
            return text.charAt(at);
        }

        private char next() {
            char c = peek();
            at++;
            return c;
        }

        private void expect(char expected) {
            char c = next();
            if (c != expected) {
                throw new IllegalArgumentException("expected " + expected + " at offset " + (at - 1));
            }
        }
    }
}
