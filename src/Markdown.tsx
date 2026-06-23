import { useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

function CopyButton({ getText, className = 'code-copy' }: { getText: () => string; className?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={className}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(getText());
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // ignore
        }
      }}
    >
      {copied ? '已复制' : '复制'}
    </button>
  );
}

// 代码块：带语言标签 + 复制按钮，外观像代码（深浅配色自适应）。
function PreBlock(props: { children?: React.ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  return (
    <div className="code-block">
      <div className="code-block-bar">
        <span className="code-dots" aria-hidden>
          <i /> <i /> <i />
        </span>
        <CopyButton getText={() => ref.current?.innerText ?? ''} />
      </div>
      <pre ref={ref}>{props.children}</pre>
    </div>
  );
}

export function Markdown({ children }: { children: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ pre: PreBlock as never }}>
      {children}
    </ReactMarkdown>
  );
}

// 整条消息复制按钮（供消息头部使用）。
export function MessageCopy({ text }: { text: string }) {
  return <CopyButton getText={() => text} className="msg-copy" />;
}
