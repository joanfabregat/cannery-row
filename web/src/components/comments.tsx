import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { api, unwrap } from "@/api/client";
import {
  type CommentTarget,
  commentsKey,
  projectKey,
  useCommentRevisions,
  useComments,
} from "@/api/queries";
import type { Comment } from "@/api/types";
import { useMe } from "@/auth/session";
import { Markdown } from "@/components/markdown";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { usePermissions } from "@/projects/use-permissions";
import { usePeople } from "@/projects/use-people";

/**
 * Discussion on a hypothesis or an attempt. Members and researchers comment;
 * authors edit their own comments, and every edit stays visible as history.
 * Comments never change a state, a verdict or a decision.
 */
export function CommentsSection({ project, target }: { project: string; target: CommentTarget }) {
  const comments = useComments(project, target);
  const { canComment } = usePermissions();
  return (
    <Section
      title="Comments"
      description="Discussion only: comments never change a status or a decision."
    >
      <div className="flex flex-col gap-5">
        <QueryView query={comments}>
          {(page) =>
            page.items.length === 0 ? (
              <EmptyState>No comment yet.</EmptyState>
            ) : (
              <ol className="flex flex-col gap-4">
                {[...page.items].reverse().map((comment) => (
                  <CommentItem
                    key={comment.id}
                    project={project}
                    target={target}
                    comment={comment}
                  />
                ))}
              </ol>
            )
          }
        </QueryView>
        {canComment ? <NewComment project={project} target={target} /> : null}
      </div>
    </Section>
  );
}

function NewComment({ project, target }: { project: string; target: CommentTarget }) {
  const id = useId();
  const [body, setBody] = useState("");
  const queryClient = useQueryClient();
  const add = useMutation({
    mutationFn: async (text: string) => {
      if (target.sequence === undefined) {
        return unwrap(
          await api.POST("/api/projects/{slug}/hypotheses/{number}/comments", {
            params: { path: { slug: project, number: target.number } },
            body: { body_markdown: text },
          }),
        );
      }
      return unwrap(
        await api.POST("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments", {
          params: { path: { slug: project, number: target.number, sequence: target.sequence } },
          body: { body_markdown: text },
        }),
      );
    },
    onSuccess: async () => {
      setBody("");
      await queryClient.invalidateQueries({ queryKey: commentsKey(project, target) });
      // Mentions add backlinks to other hypotheses.
      await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    },
  });
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (body.trim()) add.mutate(body);
      }}
    >
      <label htmlFor={id} className="text-sm font-medium">
        Add a comment
      </label>
      <Textarea
        id={id}
        value={body}
        onChange={(event) => {
          setBody(event.target.value);
        }}
        placeholder="Markdown is supported. Mention a hypothesis with #12."
      />
      {add.isError ? (
        <p role="alert" className="text-sm text-status-danger">
          {describeError(add.error)}
        </p>
      ) : null}
      <div>
        <Button type="submit" disabled={!body.trim() || add.isPending}>
          {add.isPending ? "Adding…" : "Add comment"}
        </Button>
      </div>
    </form>
  );
}

function CommentItem({
  project,
  target,
  comment,
}: {
  project: string;
  target: CommentTarget;
  comment: Comment;
}) {
  const { data: me } = useMe();
  const { canComment } = usePermissions();
  const person = usePeople();
  const [editing, setEditing] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const mine = canComment && me?.user?.id === comment.author_user_id;
  return (
    <li className="flex flex-col gap-2 rounded-md border p-4">
      <p className="text-xs text-muted-foreground">
        <span className="font-medium text-foreground">{person(comment.author_user_id)}</span> ·{" "}
        {formatDateTime(comment.created_at)}
        {comment.attempt_ref && target.sequence === undefined
          ? ` · on attempt ${comment.attempt_ref}`
          : ""}
        {comment.edited_at ? ` · edited ${formatDateTime(comment.edited_at)}` : ""}
      </p>
      {editing ? (
        <EditComment
          project={project}
          target={target}
          comment={comment}
          done={() => {
            setEditing(false);
          }}
        />
      ) : (
        <Markdown>{comment.body_markdown}</Markdown>
      )}
      <div className="flex flex-wrap gap-2">
        {mine && !editing ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setEditing(true);
            }}
          >
            Edit
          </Button>
        ) : null}
        {comment.revision > 1 ? (
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={showHistory}
            onClick={() => {
              setShowHistory(!showHistory);
            }}
          >
            {showHistory ? "Hide history" : "Show history"}
          </Button>
        ) : null}
      </div>
      {showHistory ? <CommentHistory project={project} commentId={comment.id} /> : null}
    </li>
  );
}

function CommentHistory({ project, commentId }: { project: string; commentId: string }) {
  const revisions = useCommentRevisions(project, commentId, true);
  return (
    <QueryView query={revisions}>
      {(page) => (
        <ol aria-label="Earlier versions" className="flex flex-col gap-3 border-l-2 pl-4">
          {page.items.map((revision) => (
            <li key={revision.revision} className="flex flex-col gap-1">
              <p className="text-xs text-muted-foreground">
                Version {revision.revision} · {formatDateTime(revision.created_at)}
              </p>
              <Markdown>{revision.body_markdown}</Markdown>
            </li>
          ))}
        </ol>
      )}
    </QueryView>
  );
}

function EditComment({
  project,
  target,
  comment,
  done,
}: {
  project: string;
  target: CommentTarget;
  comment: Comment;
  done: () => void;
}) {
  const id = useId();
  const [body, setBody] = useState(comment.body_markdown);
  const queryClient = useQueryClient();
  const save = useMutation({
    mutationFn: async (text: string) =>
      unwrap(
        await api.PUT("/api/projects/{slug}/comments/{comment_id}", {
          params: { path: { slug: project, comment_id: comment.id } },
          body: { expected_revision: comment.revision, body_markdown: text },
        }),
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: commentsKey(project, target) });
      await queryClient.invalidateQueries({
        queryKey: [...projectKey(project), "comment", comment.id],
      });
      done();
    },
  });
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (body.trim()) save.mutate(body);
      }}
    >
      <label htmlFor={id} className="sr-only">
        Edit your comment
      </label>
      <Textarea
        id={id}
        value={body}
        onChange={(event) => {
          setBody(event.target.value);
        }}
      />
      {save.isError ? (
        <p role="alert" className="text-sm text-status-danger">
          {describeError(save.error)}
        </p>
      ) : null}
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={!body.trim() || save.isPending}>
          Save
        </Button>
        <Button type="button" size="sm" variant="outline" onClick={done}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
