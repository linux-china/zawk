import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

type FeatureItem = {
  title: string;
  description: ReactNode;
  code: string;
  to: string;
};

const FeatureList: FeatureItem[] = [
  {
    title: 'Fast by design',
    description: (
      <>
        Type inference and a Cranelift JIT turn AWK into machine code, with SIMD CSV parsing and parallel
        execution. Usually 2-8x faster than gawk on large files.
      </>
    ),
    code: `zawk -i csv -p r -j 8 '{ s += $3 } END { print s }' big.csv`,
    to: '/docs/syntax/structured-data#parallel-execution',
  },
  {
    title: 'Structured data, by name',
    description: (
      <>
        RFC 4180 CSV, TSV, JSON Lines, Parquet, Excel and Markdown tables, with columns by name through{' '}
        <code>$FI["col"]</code>. Write CSV, JSON Lines, Parquet or Markdown.
      </>
    ),
    code: `zawk -i jsonl -o md '{ print $FI["name"], $FI["age"] }' users.jsonl`,
    to: '/docs/syntax/structured-data',
  },
  {
    title: 'A real standard library',
    description: (
      <>
        ~190 functions: text, JSON/XML/HTML, dates, crypto and JWT, IDs, statistics, HTTP, email and S3. Failures
        warn and set <code>ERRNO</code> instead of crashing.
      </>
    ),
    code: `{ print url($1)["host"], json_value($2, "$.id"), uuid("v7") }`,
    to: '/docs/stdlib/overview',
  },
  {
    title: 'Databases as input',
    description: (
      <>
        Read a SQLite, MySQL or PostgreSQL table or query like a CSV file, and write back with bound parameters.
        Redis and NATS KV are one call away.
      </>
    ),
    code: `zawk '{ print $FI["name"] }' 'postgres://u:p@host/db#people'`,
    to: '/docs/tutorials/database',
  },
  {
    title: 'Streams and messages',
    description: (
      <>
        Subscribe to a NATS subject as input, then publish to NATS or MQTT. A ten-line AWK script becomes a
        message processor.
      </>
    ),
    code: `zawk -i json '{ publish(out, MSG_BODY) }' 'nats://localhost:4222/logs.>'`,
    to: '/docs/tutorials/nats',
  },
  {
    title: 'Made for AI agents',
    description: (
      <>
        <code>zawk llm</code> writes the prompt, <code>zawk lint</code> checks the answer as JSON, and{' '}
        <code>jev</code> asks an LLM typed questions per record. A Claude Code skill is included.
      </>
    ),
    code: `zawk llm -i csv -H sales.csv 'top 3 regions?' | claude -p`,
    to: '/docs/tutorials/ai',
  },
];

function Feature({title, description, code, to}: FeatureItem) {
  return (
    <Link to={to} className={styles.card}>
      <Heading as="h3" className={styles.cardTitle}>
        {title}
      </Heading>
      <p className={styles.cardText}>{description}</p>
      <code className={styles.cardCode}>{code}</code>
    </Link>
  );
}

export default function HomepageFeatures(): ReactNode {
  return (
    <section className={styles.features}>
      <div className="container">
        <Heading as="h2" className={styles.heading}>
          Everything AWK was missing
        </Heading>
        <div className={styles.grid}>
          {FeatureList.map((props) => (
            <Feature key={props.title} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
