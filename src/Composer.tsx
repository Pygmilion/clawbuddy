import { useRef, useState } from 'react';
import { ModelSwitcher } from './ModelSwitcher';

export interface Attachment {
  name: string;
  mimeType: string;
  dataBase64: string;
}

export function Composer({
  onSend,
  loading,
}: {
  onSend: (text: string, attachments: Attachment[]) => void;
  loading: boolean;
}) {
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [dragging, setDragging] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  const canSend = (!!input.trim() || attachments.length > 0) && !loading;

  const submit = () => {
    if (!canSend) return;
    onSend(input.trim(), attachments);
    setInput('');
    setAttachments([]);
  };

  const fileToAttachment = (file: File): Promise<Attachment> =>
    new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const result = String(reader.result || '');
        const base64 = result.includes(',') ? result.split(',')[1] : result;
        resolve({
          name: file.name || 'file',
          mimeType: file.type || 'application/octet-stream',
          dataBase64: base64,
        });
      };
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(file);
    });

  const addFiles = async (files: FileList | File[]) => {
    const atts = await Promise.all(Array.from(files).map(fileToAttachment));
    setAttachments((cur) => [...cur, ...atts]);
  };

  return (
    <div className="composer">
      <div
        className={`composer-box ${dragging ? 'dragging' : ''}`}
        onDragEnter={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={(e) => {
          e.preventDefault();
          if (e.currentTarget === e.target) setDragging(false);
        }}
        onDrop={(e) => {
          e.preventDefault();
          setDragging(false);
          if (e.dataTransfer.files.length) addFiles(e.dataTransfer.files);
        }}
      >
        {attachments.length > 0 && (
          <div className="composer-attachments">
            {attachments.map((a, i) => (
              <span key={i} className="attachment-chip">
                📎 {a.name}
                <button type="button" onClick={() => setAttachments((cur) => cur.filter((_, j) => j !== i))}>
                  ×
                </button>
              </span>
            ))}
          </div>
        )}

        <textarea
          value={input}
          onChange={(e) => setInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
          }}
          onPaste={(e) => {
            const files = Array.from(e.clipboardData.files);
            if (files.length) {
              e.preventDefault();
              addFiles(files);
            }
          }}
          placeholder="今天帮你做些什么？可粘贴 / 拖拽附件；Enter 发送，Shift+Enter 换行"
          rows={4}
          disabled={loading}
        />

        <div className="composer-bar">
          <div className="composer-tools">
            <ModelSwitcher />
            <button type="button" title="附件" onClick={() => fileRef.current?.click()}>＋</button>
          </div>
          <button type="button" className="send-btn" onClick={submit} disabled={!canSend} title="发送 (Enter)">
            {loading ? '…' : '↑'}
          </button>
        </div>

        <input
          ref={fileRef}
          type="file"
          multiple
          hidden
          onChange={(e) => {
            if (e.target.files) addFiles(e.target.files);
            e.target.value = '';
          }}
        />
      </div>
      <div className="composer-hint">内容由 AI 生成，请核实重要信息</div>
    </div>
  );
}
