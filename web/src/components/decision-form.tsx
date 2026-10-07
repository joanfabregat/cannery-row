import { useMutation } from "@tanstack/react-query";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogTitle,
} from "@/components/ui/dialog";
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";

export interface DecisionChoice<Action extends string = string> {
  action: Action;
  /** The button's words: "Accept", "Ask for changes". */
  label: string;
  /** What will happen, in plain words, for the confirmation. */
  effect: string;
  variant?: "default" | "outline" | "destructive";
}

function newKey(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

/**
 * The decision part of a review screen: a required reason, one button per
 * possible decision (disabled until a reason is typed), and a confirmation
 * that says what will happen. Each confirmation carries its own idempotency
 * key, kept when the same submission is retried after an error.
 */
export function DecisionForm<Action extends string>({
  subject,
  choices,
  submit,
  onDone,
  onStale,
}: {
  /** "#12", named in the confirmation's title. */
  subject: string;
  choices: DecisionChoice<Action>[];
  submit: (action: Action, reason: string, idempotencyKey: string) => Promise<unknown>;
  onDone: (choice: DecisionChoice<Action>) => Promise<void> | void;
  /** Reload after a stale revision or a conflict. */
  onStale: () => Promise<void> | void;
}) {
  const reasonId = useId();
  const hintId = useId();
  const [reason, setReason] = useState("");
  const [chosen, setChosen] = useState<DecisionChoice<Action> | null>(null);
  const [key, setKey] = useState("");
  const ready = reason.trim().length > 0;

  const decide = useMutation({
    mutationFn: async (choice: DecisionChoice<Action>) => submit(choice.action, reason.trim(), key),
    onSuccess: async (_, choice) => {
      setChosen(null);
      await onDone(choice);
    },
  });
  const stale =
    decide.error instanceof ApiError &&
    (decide.error.code === "stale_revision" || decide.error.code === "conflict");

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <label htmlFor={reasonId} className="font-medium">
          Reason <span className="font-normal text-muted-foreground">(required)</span>
        </label>
        <p id={hintId} className="text-sm text-muted-foreground">
          Say why, in words a colleague will understand later. The reason is recorded with your
          decision and cannot be edited.
        </p>
        <Textarea
          id={reasonId}
          aria-describedby={hintId}
          required
          value={reason}
          onChange={(event) => {
            setReason(event.target.value);
          }}
          className="min-h-28"
        />
      </div>
      <div className="flex flex-wrap gap-2">
        {choices.map((choice) => (
          <Button
            key={choice.action}
            variant={choice.variant ?? "outline"}
            disabled={!ready}
            onClick={() => {
              decide.reset();
              setKey(newKey());
              setChosen(choice);
            }}
          >
            {choice.label}
          </Button>
        ))}
      </div>
      {!ready ? (
        <p className="text-sm text-muted-foreground">Type a reason to enable the decisions.</p>
      ) : null}
      <Dialog
        open={chosen !== null}
        onOpenChange={(open) => {
          if (!open && !decide.isPending) setChosen(null);
        }}
      >
        {chosen ? (
          <DialogContent closeLabel="Cancel">
            <DialogTitle>
              {chosen.label}: {subject}?
            </DialogTitle>
            <DialogDescription>{chosen.effect}</DialogDescription>
            <div className="rounded-md bg-muted p-3 text-sm">
              <p className="font-medium">Your reason</p>
              <p className="mt-1 whitespace-pre-wrap">{reason.trim()}</p>
            </div>
            {decide.isError ? (
              <div role="alert" className="flex flex-col gap-2 text-sm text-status-danger">
                <p>{describeError(decide.error)}</p>
                {stale ? (
                  <div>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setChosen(null);
                        void onStale();
                      }}
                    >
                      Reload the latest version
                    </Button>
                  </div>
                ) : null}
              </div>
            ) : null}
            <DialogFooter>
              <Button
                variant="outline"
                disabled={decide.isPending}
                onClick={() => {
                  setChosen(null);
                }}
              >
                Go back
              </Button>
              <Button
                variant={chosen.variant === "destructive" ? "destructive" : "default"}
                disabled={decide.isPending}
                onClick={() => {
                  decide.mutate(chosen);
                }}
              >
                {decide.isPending ? "Recording…" : `Confirm: ${chosen.label}`}
              </Button>
            </DialogFooter>
          </DialogContent>
        ) : null}
      </Dialog>
    </div>
  );
}
