import { useMutation } from "@tanstack/react-query";
import { CheckIcon, CopyIcon } from "lucide-react";
import { useId, useState } from "react";

import { api, unwrap } from "@/api/client";
import type { Token, TokenCreated } from "@/api/types";
import { StatusChip } from "@/components/status-chip";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { NativeSelect } from "@/components/ui/native-select";
import { describeError } from "@/lib/errors";
import { formatDate, formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { MAX_TOKEN_NAME, tokenNameProblem, tokenStatus } from "@/lib/tokens";

const EXPIRY_CHOICES = [7, 30, 90, 365];

/** Shown once after creation: the secret with a copy button. */
export function CreatedSecret({
  created,
  onClose,
}: {
  created: TokenCreated;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const id = useId();
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm">
        Copy the token <strong>{created.name}</strong> now: it is shown only this once and cannot be
        read again.
      </p>
      <label htmlFor={id} className="sr-only">
        The new token
      </label>
      <Input
        id={id}
        readOnly
        value={created.token}
        className="font-mono text-xs"
        onFocus={(e) => {
          e.target.select();
        }}
      />
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          onClick={() => {
            void navigator.clipboard.writeText(created.token).then(() => {
              setCopied(true);
            });
          }}
        >
          {copied ? <CheckIcon aria-hidden="true" /> : <CopyIcon aria-hidden="true" />}
          {copied ? "Copied" : "Copy the token"}
        </Button>
        <Button type="button" variant="outline" onClick={onClose}>
          Done
        </Button>
      </div>
      <p role="status" className="sr-only">
        {copied ? "The token was copied." : ""}
      </p>
    </div>
  );
}

/**
 * The form for a new token, shared by personal and service tokens:
 * name, scopes and lifetime; then the secret, once.
 */
export function NewTokenDialog({
  trigger,
  title,
  description,
  create,
  onCreated,
}: {
  trigger: string;
  title: string;
  description: string;
  create: (body: {
    name: string;
    scopes: ("read" | "write")[];
    expires_in_days: number;
  }) => Promise<TokenCreated>;
  onCreated: () => Promise<void> | void;
}) {
  const nameId = useId();
  const nameHint = useId();
  const expiryId = useId();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [read, setRead] = useState(true);
  const [write, setWrite] = useState(false);
  const [days, setDays] = useState(90);
  // The secret lives only in this state, cleared when the dialog closes: the
  // mutation returns nothing and leaves the cache at once, so the query
  // client never holds it.
  const [created, setCreated] = useState<TokenCreated | null>(null);
  const mutation = useMutation({
    gcTime: 0,
    mutationFn: async (): Promise<void> => {
      const token = await create({
        name: name.trim(),
        scopes: [...(read ? (["read"] as const) : []), ...(write ? (["write"] as const) : [])],
        expires_in_days: days,
      });
      setCreated(token);
    },
    onSuccess: async () => {
      await onCreated();
    },
  });
  const problem = tokenNameProblem(name);
  const reset = () => {
    setName("");
    setRead(true);
    setWrite(false);
    setDays(90);
    setCreated(null);
    mutation.reset();
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) reset();
      }}
    >
      <DialogTrigger asChild>
        <Button>{trigger}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
        {created ? (
          <CreatedSecret
            created={created}
            onClose={() => {
              setOpen(false);
              reset();
            }}
          />
        ) : (
          <form
            className="flex flex-col gap-4"
            onSubmit={(event) => {
              event.preventDefault();
              if (problem === null && (read || write)) mutation.mutate();
            }}
          >
            <div className="flex flex-col gap-1.5">
              <label htmlFor={nameId} className="text-sm font-medium">
                Name
              </label>
              <p id={nameHint} className="text-xs text-muted-foreground">
                Say where it is used, for example “laptop CLI”. Its actions are labelled with it.
              </p>
              <Input
                id={nameId}
                value={name}
                maxLength={MAX_TOKEN_NAME}
                aria-describedby={nameHint}
                aria-invalid={name !== "" && problem !== null}
                onChange={(event) => {
                  setName(event.target.value);
                }}
              />
              {name !== "" && problem !== null ? (
                <p className="text-xs text-status-danger">{problem}</p>
              ) : null}
            </div>
            <fieldset className="flex flex-col gap-1.5">
              <legend className="mb-1 text-sm font-medium">What it can do</legend>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  className="size-4 accent-primary"
                  checked={read}
                  onChange={(event) => {
                    setRead(event.target.checked);
                  }}
                />
                {label("scope", "read")}: see what you can see
              </label>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  className="size-4 accent-primary"
                  checked={write}
                  onChange={(event) => {
                    setWrite(event.target.checked);
                  }}
                />
                {label("scope", "write")}: act as you (comment, decide, create)
              </label>
            </fieldset>
            <div className="flex flex-col gap-1.5">
              <label htmlFor={expiryId} className="text-sm font-medium">
                Expires after
              </label>
              <NativeSelect
                id={expiryId}
                value={days}
                onChange={(event) => {
                  setDays(Number(event.target.value));
                }}
              >
                {EXPIRY_CHOICES.map((d) => (
                  <option key={d} value={d}>
                    {d} days
                  </option>
                ))}
              </NativeSelect>
            </div>
            {mutation.isError ? (
              <p role="alert" className="text-sm text-status-danger">
                {describeError(mutation.error)}
              </p>
            ) : null}
            <DialogFooter>
              <Button
                type="submit"
                disabled={problem !== null || !(read || write) || mutation.isPending}
              >
                Create token
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function RevokeTokenButton({
  token,
  onRevoked,
}: {
  token: Token;
  onRevoked: () => Promise<void> | void;
}) {
  const [open, setOpen] = useState(false);
  const revoke = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.DELETE("/api/tokens/{token_id}", { params: { path: { token_id: token.id } } }),
      ),
    onSuccess: async () => {
      setOpen(false);
      await onRevoked();
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) revoke.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline" size="sm">
          Revoke <span className="sr-only">{token.name}</span>
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Revoke {token.name}?</DialogTitle>
        <DialogDescription>
          Anything using this token stops working at once. This cannot be undone; you can create a
          new token instead.
        </DialogDescription>
        {revoke.isError ? (
          <p role="alert" className="text-sm text-status-danger">
            {describeError(revoke.error)}
          </p>
        ) : null}
        <DialogFooter>
          <Button
            variant="destructive"
            disabled={revoke.isPending}
            onClick={() => {
              revoke.mutate();
            }}
          >
            Revoke the token
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function TokenRows({
  tokens,
  canRevoke = true,
  onRevoked,
}: {
  tokens: Token[];
  /** False hides the Revoke buttons (a session without the write scope). */
  canRevoke?: boolean;
  onRevoked: () => Promise<void> | void;
}) {
  return (
    <ul className="flex flex-col divide-y">
      {tokens.map((token) => {
        const status = tokenStatus(token);
        return (
          <li key={token.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
            <div className="flex flex-col gap-0.5">
              <span className="font-medium">{token.name}</span>
              <span className="text-xs text-muted-foreground">
                <code>{token.display_prefix}…</code> ·{" "}
                {token.scopes.map((s) => label("scope", s)).join(" and ")} · created{" "}
                {formatDate(token.created_at)} · expires {formatDate(token.expires_at)}
                {token.last_used_at
                  ? ` · last used ${formatDateTime(token.last_used_at)}`
                  : " · never used"}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <StatusChip domain="token" value={status} />
              {canRevoke && status === "active" ? (
                <RevokeTokenButton token={token} onRevoked={onRevoked} />
              ) : null}
            </div>
          </li>
        );
      })}
    </ul>
  );
}
