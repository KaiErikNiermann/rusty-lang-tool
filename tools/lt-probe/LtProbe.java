// Run LanguageTool on sentences and print its matches and token analysis, for `cargo xtask coverage`.
//
// Reads one JSON object {"id", "text"} per line on stdin and prints one JSON line per sentence:
//   {"id", "matches": [{"rule", "full_id", "java", "from", "to", "repl": [...]}],
//    "tokens": [{"text", "readings": ["lemma/TAG", ...]}]}
// `java` is true for rules implemented in Java rather than as XML patterns. Readings are those left
// after LT's disambiguation; an untagged reading prints as "null/null". The language is LT's default
// variant of the short code given as the only argument (de -> de-DE, en -> en-US).
//
// Run with the runtime `cargo xtask coverage` resolves (see build.gradle):
//   java -cp 'tools/lt-runtime/<ver>/<lang>/*' tools/lt-probe/LtProbe.java de < in.jsonl > out.jsonl

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import org.languagetool.AnalyzedSentence;
import org.languagetool.AnalyzedToken;
import org.languagetool.AnalyzedTokenReadings;
import org.languagetool.JLanguageTool;
import org.languagetool.Language;
import org.languagetool.Languages;
import org.languagetool.rules.RuleMatch;
import org.languagetool.rules.patterns.AbstractPatternRule;

public class LtProbe {
  public static void main(String[] args) throws Exception {
    Language lang = Languages.getLanguageForShortCode(args[0]);
    if (lang.getDefaultLanguageVariant() != null) {
      lang = lang.getDefaultLanguageVariant();
    }
    JLanguageTool lt = new JLanguageTool(lang);
    ObjectMapper json = new ObjectMapper();
    PrintStream out = new PrintStream(System.out, true, StandardCharsets.UTF_8);
    BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
    for (String line; (line = in.readLine()) != null; ) {
      JsonNode req = json.readTree(line);
      String text = req.get("text").asText();
      ObjectNode row = json.createObjectNode();
      row.put("id", req.get("id").asText());
      ArrayNode matches = row.putArray("matches");
      for (RuleMatch m : lt.check(text)) {
        ObjectNode o = matches.addObject();
        boolean pattern = m.getRule() instanceof AbstractPatternRule;
        o.put("rule", m.getRule().getId());
        o.put("full_id", pattern ? ((AbstractPatternRule) m.getRule()).getFullId() : m.getRule().getId());
        o.put("java", !pattern);
        o.put("from", m.getFromPos());
        o.put("to", m.getToPos());
        ArrayNode repl = o.putArray("repl");
        m.getSuggestedReplacements().forEach(repl::add);
      }
      ArrayNode tokens = row.putArray("tokens");
      for (AnalyzedSentence s : lt.analyzeText(text)) {
        for (AnalyzedTokenReadings t : s.getTokensWithoutWhitespace()) {
          ObjectNode o = tokens.addObject();
          o.put("text", t.getToken());
          ArrayNode r = o.putArray("readings");
          for (AnalyzedToken a : t.getReadings()) {
            r.add(a.getLemma() + "/" + a.getPOSTag());
          }
        }
      }
      out.println(json.writeValueAsString(row));
    }
  }
}
