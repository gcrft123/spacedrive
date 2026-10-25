import clsx from "clsx";
import type { File as FileType } from "@sd/ts-client";
import { isVirtualFile } from "@sd/ts-client";
import { useCompactLayout } from "../../../hooks/useCompactLayout";
import { useOptionalExplorer } from "../context";
import { Thumb } from "./Thumb";
import { Title } from "./Title";
import { Metadata } from "./Metadata";

interface FileProps {
	file: FileType;
	selected?: boolean;
	onClick?: (e: React.MouseEvent) => void;
	onDoubleClick?: (e: React.MouseEvent) => void;
	onContextMenu?: (e: React.MouseEvent) => void;
	onMouseDown?: (e: React.MouseEvent) => void;
	onMouseMove?: (e: React.MouseEvent) => void;
	onMouseUp?: (e: React.MouseEvent) => void;
	onMouseLeave?: (e: React.MouseEvent) => void;
	layout?: "column" | "row";
	children?: React.ReactNode;
	className?: string;
	"data-file-id"?: string;
}

function FileComponent({
	file: _file,
	selected: _selected,
	onClick,
	onDoubleClick,
	onContextMenu,
	onMouseDown,
	onMouseMove,
	onMouseUp,
	onMouseLeave,
	layout = "column",
	children,
	className,
	"data-file-id": dataFileId,
}: FileProps) {
	const compact = useCompactLayout();
	const explorer = useOptionalExplorer();

	// Phones send no reliable double-tap, so a tap opens folders and shows a
	// file's details (preview, share, download) in the inspector.
	const handleClick = (e: React.MouseEvent) => {
		onClick?.(e);
		if (!compact || !explorer) return;
		if (_file.kind === "Directory" || isVirtualFile(_file)) {
			onDoubleClick?.(e);
		} else {
			explorer?.setInspectorVisible(true);
		}
	};

	return (
		<div
			data-file-id={dataFileId}
			onClick={handleClick}
			onDoubleClick={onDoubleClick}
			onContextMenu={onContextMenu}
			onMouseDown={onMouseDown}
			onMouseMove={onMouseMove}
			onMouseUp={onMouseUp}
			onMouseLeave={onMouseLeave}
			tabIndex={-1}
			className={clsx(
				"cursor-default transition-colors outline-none focus:outline-none",
				layout === "column"
					? "flex flex-col"
					: "flex flex-row items-center",
				className,
			)}
		>
			{children}
		</div>
	);
}

export const File = Object.assign(FileComponent, {
	Thumb,
	Title,
	Metadata,
});