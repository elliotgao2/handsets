package dev.handsets.daemon;

public final class JsonOut {
    private final StringBuilder sb;

    public JsonOut(StringBuilder sb) {
        this.sb = sb;
    }

    public JsonOut beginObj() { sb.append('{'); return this; }
    public JsonOut endObj()   { trimComma(); sb.append('}').append(','); return this; }
    public JsonOut beginArr() { sb.append('['); return this; }
    public JsonOut endArr()   { trimComma(); sb.append(']').append(','); return this; }

    public JsonOut key(String k) {
        sb.append('"').append(k).append('"').append(':');
        return this;
    }

    public JsonOut str(String k, CharSequence v) {
        if (v == null || v.length() == 0) return this;
        key(k);
        escape(v);
        sb.append(',');
        return this;
    }

    public JsonOut num(String k, long v) {
        key(k).append(Long.toString(v)).append(',');
        return this;
    }

    public JsonOut bool(String k, boolean v) {
        if (!v) return this;
        key(k).append("true").append(',');
        return this;
    }

    public JsonOut rect(String k, int l, int t, int r, int b) {
        key(k).append('[')
              .append(l).append(',')
              .append(t).append(',')
              .append(r).append(',')
              .append(b).append("],");
        return this;
    }

    public JsonOut rawKey(String k, String rawJson) {
        key(k).append(rawJson).append(',');
        return this;
    }

    public StringBuilder append(String s)  { return sb.append(s); }
    public StringBuilder append(char c)    { return sb.append(c); }
    public StringBuilder append(int v)     { return sb.append(v); }

    public void trimComma() {
        int n = sb.length();
        if (n > 0 && sb.charAt(n - 1) == ',') sb.setLength(n - 1);
    }

    public String finish() {
        trimComma();
        return sb.toString();
    }

    private void escape(CharSequence v) {
        sb.append('"');
        int n = v.length();
        for (int i = 0; i < n; i++) {
            char c = v.charAt(i);
            switch (c) {
                case '"':  sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n");  break;
                case '\r': sb.append("\\r");  break;
                case '\t': sb.append("\\t");  break;
                case '\b': sb.append("\\b");  break;
                case '\f': sb.append("\\f");  break;
                default:
                    if (c < 0x20) {
                        sb.append("\\u").append(String.format("%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
            }
        }
        sb.append('"');
    }
}
