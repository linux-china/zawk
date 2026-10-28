import type {ReactNode} from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import CodeBlock from '@theme/CodeBlock';
import HomepageFeatures from '@site/src/components/HomepageFeatures';

import styles from './index.module.css';

function Terminal(): ReactNode {
  return (
    <div className={styles.terminal} aria-label="zawk terminal example">
      <div className={styles.terminalBar}>
        <span />
        <span />
        <span />
      </div>
      <pre className={styles.terminalBody}>
        <span className={styles.prompt}>$ </span>
        zawk '$FI["age"] &gt; 20 {'{'} print $FI["name"], $FI["age"] {'}'}' \{'\n'}
        {'    '}'mysql://root:***@127.0.0.1:3306/test#people'{'\n'}
        <span className={styles.out}>Jackie 48{'\n'}Love 48{'\n'}Leo 24{'\n'}</span>
        {'\n'}
        <span className={styles.prompt}>$ </span>
        zawk -i parquet 'NR &lt;= 2' people.parquet{'\n'}
        <span className={styles.out}>
          {'{'}"id":1,"name":"Jackie","age":48{'}'}{'\n'}{'{'}"id":2,"name":"Love","age":48{'}'}{'\n'}
        </span>
        {'\n'}
        <span className={styles.prompt}>$ </span>
        echo hello | zawk '{'{'} print ulid(), encode("base64", $0), mask("abc@example.com") {'}'}'{'\n'}
        <span className={styles.out}>01M60KH8PG6NM4HE8ZKQKXSAXT aGVsbG8= abc*********com{'\n'}</span>
      </pre>
    </div>
  );
}

function HomepageHeader() {
  return (
    <header className={styles.hero}>
      <div className={clsx('container', styles.heroInner)}>
        <div className={styles.heroText}>
          <p className={styles.eyebrow}>AWK + stdlib + Rust</p>
          <Heading as="h1" className={styles.title}>
            The AWK you know,
            <br />
            for the data you have <span className={styles.accent}>now</span>.
          </Heading>
          <p className={styles.subtitle}>
            zawk runs your AWK programs, compiled with Cranelift, and adds CSV, JSON, Parquet, Excel,
            databases, S3 and NATS as input, ~190 standard library functions, modern syntax, and tooling
            built for AI agents.
          </p>
          <div className={styles.buttons}>
            <Link className={clsx('button button--lg', styles.primaryButton)} to="/docs/intro">
              Get Started
            </Link>
            <Link className={clsx('button button--lg', styles.secondaryButton)} to="/blog/introducing-zawk">
              Why zawk?
            </Link>
          </div>
          <CodeBlock language="bash" className={styles.install}>
            brew install linux-china/tap/zawk
          </CodeBlock>
        </div>
        <Terminal />
      </div>
    </header>
  );
}

const sources = [
  'CSV / TSV',
  'JSON Lines',
  'Parquet',
  'Excel',
  'Markdown',
  'SQLite',
  'MySQL',
  'PostgreSQL',
  'S3',
  'HTTP',
  'NATS',
  'gz · zst · xz',
];

function Sources(): ReactNode {
  return (
    <section className={styles.sources}>
      <div className="container">
        <p className={styles.sourcesTitle}>Reads and writes</p>
        <ul className={styles.sourceList}>
          {sources.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ul>
      </div>
    </section>
  );
}

const syntaxSamples: {label: string; value: string; code: string}[] = [
  {
    label: 'f-strings',
    value: 'fstring',
    code: `{ print f"{NR:>4}: {toupper($2):<10} {$3 * 1.1:,.2f}" }
{ sql = f"SELECT * FROM {table} WHERE id = {$1}" }`,
  },
  {
    label: 'match',
    value: 'match',
    code: `{
    level = match $3 {
        /^E/ => "error",
        "warn" | "W" => "warn",
        /(\\d+)ms/ as m => m[1] > 1000 ? "slow" : "ok",
        _ => "info"
    }
}`,
  },
  {
    label: 'try / ??',
    value: 'try',
    code: `{
    name = $FI["nick"] ?? "Guest"
    body = try read_all($FI["url"]) else ""
    rows = try pg_query(db, sql) else fail(ERRNO)
}`,
  },
  {
    label: 'local & loops',
    value: 'loops',
    code: `{ local total = 0, seen[] }
$1 !in seen { seen[$1]; print $(-1) }
END {
    for (word, n in count) print word, n
    for (i in 1..=10) print i
}`,
  },
];

function SyntaxShowcase(): ReactNode {
  return (
    <section className={styles.showcase}>
      <div className={clsx('container', styles.showcaseInner)}>
        <div className={styles.showcaseText}>
          <Heading as="h2">Modern syntax, still AWK</Heading>
          <p>
            f-strings, raw strings, <code>??</code>, <code>try … else</code>, <code>match</code> expressions,
            <code>local</code> variables, key-value loops, ranges, <code>!in</code> and negative field
            indexes. Existing AWK programs keep their meaning.
          </p>
          <Link to="/docs/syntax/overview">Explore the syntax →</Link>
        </div>
        <div className={styles.showcaseCode}>
          <Tabs>
            {syntaxSamples.map((s) => (
              <TabItem key={s.value} value={s.value} label={s.label}>
                <CodeBlock language="awk">{s.code}</CodeBlock>
              </TabItem>
            ))}
          </Tabs>
        </div>
      </div>
    </section>
  );
}

function CallToAction(): ReactNode {
  return (
    <section className={styles.cta}>
      <div className="container">
        <Heading as="h2">Start with a one-liner</Heading>
        <p>Install zawk and point it at a file you already have.</p>
        <div className={styles.buttons}>
          <Link className={clsx('button button--lg', styles.primaryButton)} to="/docs/intro">
            Read the docs
          </Link>
          <Link className={clsx('button button--lg', styles.secondaryButton)} to="/docs/tutorials/database">
            Browse tutorials
          </Link>
        </div>
      </div>
    </section>
  );
}

export default function Home(): ReactNode {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout title="Modern AWK + stdlib + Rust" description={siteConfig.tagline}>
      <HomepageHeader />
      <main>
        <Sources />
        <HomepageFeatures />
        <SyntaxShowcase />
        <CallToAction />
      </main>
    </Layout>
  );
}
