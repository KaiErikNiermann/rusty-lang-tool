// Dump a LanguageTool n-gram Lucene index to `ngram<TAB>count` TSV — the Norvig format our
// confusion-model build already consumes. LT stores each n-gram as a Lucene document: the n-gram text
// is the indexed field "ngram"; its occurrence count is the stored field "count" (plus a special
// "totalTokenCount" doc we skip). We read the n-gram from the term dictionary (robust whether or not
// "ngram" is stored) and the count from the stored field.
//
// Build-time only. Compile + run with Lucene 6.x on the classpath (6.x reads the 2015 Lucene-5.0
// index via lucene-backward-codecs):
//   javac -cp tools/lib/* -d tools/out tools/NgramDump.java
//   java -cp tools/lib/*:tools/out NgramDump <index-dir> <out.tsv>

import java.io.BufferedWriter;
import java.io.FileWriter;
import java.io.PrintWriter;
import java.nio.file.Paths;

import org.apache.lucene.index.DirectoryReader;
import org.apache.lucene.index.MultiFields;
import org.apache.lucene.index.PostingsEnum;
import org.apache.lucene.index.Terms;
import org.apache.lucene.index.TermsEnum;
import org.apache.lucene.store.FSDirectory;
import org.apache.lucene.util.BytesRef;

public class NgramDump {
    public static void main(String[] args) throws Exception {
        if (args.length != 2) {
            System.err.println("usage: NgramDump <index-dir> <out.tsv>");
            System.exit(2);
        }
        long written = 0;
        try (DirectoryReader reader = DirectoryReader.open(FSDirectory.open(Paths.get(args[0])));
             PrintWriter out = new PrintWriter(new BufferedWriter(new FileWriter(args[1]), 1 << 20))) {
            Terms terms = MultiFields.getTerms(reader, "ngram");
            if (terms == null) {
                System.err.println("no 'ngram' field in " + args[0]);
                System.exit(1);
            }
            TermsEnum te = terms.iterator();
            PostingsEnum pe = null;
            BytesRef term;
            while ((term = te.next()) != null) {
                String ngram = term.utf8ToString();
                pe = te.postings(pe, PostingsEnum.NONE);
                int doc = pe.nextDoc();
                if (doc == PostingsEnum.NO_MORE_DOCS) continue;
                String count = reader.document(doc).get("count");
                if (count == null) continue;
                // The n-gram text itself never contains a tab; guard anyway.
                out.print(ngram);
                out.print('\t');
                out.print(count);
                out.print('\n');
                written++;
            }
        }
        System.err.println("wrote " + written + " n-grams to " + args[1]);
    }
}
