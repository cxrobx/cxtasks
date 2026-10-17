import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { ClipboardPaste, ImagePlus, X } from "lucide-react";
import { useState } from "react";
import { useTaskStore } from "@/stores/taskStore";
import type { Attachment, Task } from "@/types/task";

const IMAGE_EXTENSIONS = [
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "avif",
  "heic",
  "heif",
  "tif",
  "tiff",
];

export async function pickImagePaths(): Promise<string[]> {
  const picked = await open({
    multiple: true,
    directory: false,
    filters: [{ name: "Images", extensions: IMAGE_EXTENSIONS }],
  });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}

export function AttachmentGrid({
  attachments,
  onRemove,
}: {
  attachments: Attachment[];
  onRemove: (id: string) => void;
}) {
  if (attachments.length === 0) return null;
  return (
    <div className="mt-2 grid grid-cols-2 gap-2">
      {attachments.map((attachment) => (
        <figure
          key={attachment.id}
          className="group/image relative overflow-hidden rounded-md border border-border-subtle bg-input"
        >
          <img
            src={convertFileSrc(attachment.path)}
            alt={attachment.name}
            loading="lazy"
            className="h-28 w-full object-cover"
          />
          <figcaption
            className="truncate border-t border-border-subtle px-1.5 py-1 text-[10px] text-content-faint"
            title={attachment.name}
          >
            {attachment.name}
          </figcaption>
          <button
            type="button"
            aria-label={`Remove ${attachment.name}`}
            title="Remove image"
            onClick={() => onRemove(attachment.id)}
            className="absolute right-1 top-1 rounded-full bg-black/65 p-1 text-white opacity-0 transition-opacity group-hover/image:opacity-100 focus:opacity-100"
          >
            <X size={11} />
          </button>
        </figure>
      ))}
    </div>
  );
}

export function NoteAttachments({ task }: { task: Task }) {
  const attachments = useTaskStore((s) => s.attachments).filter(
    (attachment) => attachment.comment_id === null
  );
  const addNoteImages = useTaskStore((s) => s.addNoteImages);
  const pasteNoteImage = useTaskStore((s) => s.pasteNoteImage);
  const removeAttachment = useTaskStore((s) => s.removeAttachment);
  const [busy, setBusy] = useState(false);

  const choose = async () => {
    const paths = await pickImagePaths();
    if (paths.length === 0) return;
    setBusy(true);
    await addNoteImages(task.id, paths);
    setBusy(false);
  };

  return (
    <div>
      <AttachmentGrid
        attachments={attachments}
        onRemove={(id) => void removeAttachment(id)}
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={() => void choose()}
          className="mt-2 flex items-center gap-1.5 rounded-md px-1.5 py-1 text-[11px] text-content-faint hover:bg-content/6 hover:text-content disabled:opacity-50"
        >
          <ImagePlus size={13} />
          {busy ? "Adding images…" : "Add images"}
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await pasteNoteImage(task.id);
            } finally {
              setBusy(false);
            }
          }}
          title="Paste an image from the clipboard, or paste directly in Notes"
          className="mt-2 flex items-center gap-1.5 rounded-md px-1.5 py-1 text-[11px] text-content-faint hover:bg-content/6 hover:text-content disabled:opacity-50"
        >
          <ClipboardPaste size={13} />
          Paste image
        </button>
      </div>
    </div>
  );
}
