import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { api, unwrap } from "@/api/client";
import { projectKey, useMembers, useServiceAccounts, useServiceTokens } from "@/api/queries";
import type { Member, ServiceAccount } from "@/api/types";
import { EmptyState, QueryView } from "@/components/query-state";
import { Section } from "@/components/section";
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
import { Textarea } from "@/components/ui/textarea";
import { describeError } from "@/lib/errors";
import { formatDate } from "@/lib/format";
import { label } from "@/lib/labels";
import { type Project, useProjects } from "@/projects/project-context";

import { NewTokenDialog, TokenRows } from "./tokens";

/** Installation administration: projects, their members and service accounts. */

const SLUG = /^[a-z0-9][a-z0-9-]{0,62}$/;
const ROLES = ["viewer", "member", "researcher"] as const;
type Role = (typeof ROLES)[number];

function ErrorLine({ error }: { error: unknown }) {
  if (!error) return null;
  return (
    <p role="alert" className="text-sm text-status-danger">
      {describeError(error)}
    </p>
  );
}

/**
 * The administration sections. Without the `write` scope (a read-only
 * token's session) they only list: every action that changes something is
 * hidden, as the backend would refuse it.
 */
export function AdminSections({
  current,
  canWrite,
}: {
  current: Project | null;
  canWrite: boolean;
}) {
  const projects = useProjects();
  const [chosen, setChosen] = useState<string | null>(current?.slug ?? null);
  const list = projects.data ?? [];
  const slug = chosen ?? list[0]?.slug ?? null;
  const pickerId = useId();
  return (
    <>
      <Section
        title="Projects"
        description="Every project of this installation. Only administrators see this."
        actions={canWrite ? <NewProjectDialog /> : undefined}
      >
        {list.length === 0 ? (
          <EmptyState>No project yet.</EmptyState>
        ) : (
          <ul className="flex flex-col divide-y">
            {list.map((p) => (
              <li key={p.slug} className="flex flex-wrap items-center justify-between gap-2 py-2.5">
                <span>
                  <span className="font-medium">{p.title}</span>{" "}
                  <span className="text-sm text-muted-foreground">({p.slug})</span>
                </span>
                <span className="text-sm text-muted-foreground">
                  {p.role ? `You: ${label("role", p.role)}` : "You are not a member"} · created{" "}
                  {formatDate(p.created_at)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Section>
      {slug !== null ? (
        <>
          <div className="flex max-w-sm flex-col gap-1.5">
            <label htmlFor={pickerId} className="text-sm font-medium">
              Manage the members and service accounts of
            </label>
            <NativeSelect
              id={pickerId}
              value={slug}
              onChange={(event) => {
                setChosen(event.target.value);
              }}
            >
              {list.map((p) => (
                <option key={p.slug} value={p.slug}>
                  {p.title}
                </option>
              ))}
            </NativeSelect>
          </div>
          <MembersSection key={`members-${slug}`} project={slug} canWrite={canWrite} />
          <ServiceAccountsSection key={`services-${slug}`} project={slug} canWrite={canWrite} />
        </>
      ) : null}
    </>
  );
}

function NewProjectDialog() {
  const [open, setOpen] = useState(false);
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [trackSlug, setTrackSlug] = useState("");
  const [trackTitle, setTrackTitle] = useState("");
  const ids = {
    slug: useId(),
    title: useId(),
    description: useId(),
    hint: useId(),
    trackSlug: useId(),
    trackTitle: useId(),
    trackHint: useId(),
  };
  const queryClient = useQueryClient();
  const create = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects", {
          body: {
            slug,
            title: title.trim(),
            description: description.trim(),
            tracks: [{ slug: trackSlug, title: trackTitle.trim() }],
          },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setSlug("");
      setTitle("");
      setDescription("");
      setTrackSlug("");
      setTrackTitle("");
      await queryClient.invalidateQueries({ queryKey: ["projects"] });
    },
  });
  const valid =
    SLUG.test(slug) &&
    title.trim().length > 0 &&
    title.trim().length <= 200 &&
    SLUG.test(trackSlug) &&
    trackTitle.trim().length > 0;
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) create.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button>New project</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>New project</DialogTitle>
        <DialogDescription>
          A project starts with one track, a first line of research; researchers add more later. Its
          science configuration is published separately, through the API or the command line.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (valid) create.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.title} className="text-sm font-medium">
              Title
            </label>
            <Input
              id={ids.title}
              value={title}
              maxLength={200}
              onChange={(event) => {
                setTitle(event.target.value);
              }}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.slug} className="text-sm font-medium">
              Short name
            </label>
            <p id={ids.hint} className="text-xs text-muted-foreground">
              Lowercase letters, digits and dashes; used in addresses and references
              (short-name#12). It cannot change later.
            </p>
            <Input
              id={ids.slug}
              value={slug}
              aria-describedby={ids.hint}
              aria-invalid={slug !== "" && !SLUG.test(slug)}
              onChange={(event) => {
                setSlug(event.target.value);
              }}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.description} className="text-sm font-medium">
              Description
            </label>
            <Textarea
              id={ids.description}
              value={description}
              onChange={(event) => {
                setDescription(event.target.value);
              }}
            />
          </div>
          <fieldset className="flex flex-col gap-4">
            <legend className="mb-2 text-sm font-medium">First track</legend>
            <div className="flex flex-col gap-1.5">
              <label htmlFor={ids.trackTitle} className="text-sm font-medium">
                Track title
              </label>
              <Input
                id={ids.trackTitle}
                value={trackTitle}
                onChange={(event) => {
                  setTrackTitle(event.target.value);
                }}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <label htmlFor={ids.trackSlug} className="text-sm font-medium">
                Track short name
              </label>
              <p id={ids.trackHint} className="text-xs text-muted-foreground">
                Lowercase letters, digits and dashes, unique in the project (for example baselines).
              </p>
              <Input
                id={ids.trackSlug}
                value={trackSlug}
                aria-describedby={ids.trackHint}
                aria-invalid={trackSlug !== "" && !SLUG.test(trackSlug)}
                onChange={(event) => {
                  setTrackSlug(event.target.value);
                }}
              />
            </div>
          </fieldset>
          <ErrorLine error={create.error} />
          <DialogFooter>
            <Button type="submit" disabled={!valid || create.isPending}>
              Create project
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function MembersSection({ project, canWrite }: { project: string; canWrite: boolean }) {
  const members = useMembers(project);
  const queryClient = useQueryClient();
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: projectKey(project) });
    await queryClient.invalidateQueries({ queryKey: ["projects"] });
    await queryClient.invalidateQueries({ queryKey: ["me"] });
  };
  return (
    <Section
      title="Members"
      description="Who can open this project, and what each person may do there."
      actions={canWrite ? <AddMemberDialog project={project} onAdded={refresh} /> : undefined}
    >
      <QueryView query={members}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>Nobody belongs to this project yet.</EmptyState>
          ) : (
            <ul className="flex flex-col divide-y">
              {page.items.map((m) => (
                <MemberRow
                  key={m.user_id}
                  project={project}
                  member={m}
                  canWrite={canWrite}
                  onChanged={refresh}
                />
              ))}
            </ul>
          )
        }
      </QueryView>
    </Section>
  );
}

function MemberRow({
  project,
  member,
  canWrite,
  onChanged,
}: {
  project: string;
  member: Member;
  canWrite: boolean;
  onChanged: () => Promise<void>;
}) {
  const roleId = useId();
  const name = member.display_name ?? member.email ?? "A member";
  const setRole = useMutation({
    mutationFn: async (role: Role) =>
      unwrap(
        await api.PUT("/api/projects/{slug}/members/{user_id}", {
          params: { path: { slug: project, user_id: member.user_id } },
          body: { role },
        }),
      ),
    onSuccess: onChanged,
  });
  const [confirm, setConfirm] = useState(false);
  const remove = useMutation({
    mutationFn: async () => {
      await api.DELETE("/api/projects/{slug}/members/{user_id}", {
        params: { path: { slug: project, user_id: member.user_id } },
      });
    },
    onSuccess: async () => {
      setConfirm(false);
      await onChanged();
    },
  });
  return (
    <li className="flex flex-wrap items-center justify-between gap-3 py-3">
      <div className="flex flex-col">
        <span className="font-medium">{name}</span>
        {member.email && member.email !== name ? (
          <span className="text-xs text-muted-foreground">{member.email}</span>
        ) : null}
      </div>
      {canWrite ? (
        <div className="flex flex-wrap items-center gap-2">
          <label htmlFor={roleId} className="sr-only">
            Role of {name}
          </label>
          <NativeSelect
            id={roleId}
            className="w-36"
            value={member.role}
            disabled={setRole.isPending}
            onChange={(event) => {
              setRole.mutate(event.target.value as Role);
            }}
          >
            {ROLES.map((role) => (
              <option key={role} value={role}>
                {label("role", role)}
              </option>
            ))}
          </NativeSelect>
          <Dialog
            open={confirm}
            onOpenChange={(next) => {
              setConfirm(next);
              if (next) remove.reset();
            }}
          >
            <DialogTrigger asChild>
              <Button variant="outline" size="sm">
                Remove <span className="sr-only">{name}</span>
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogTitle>Remove {name}?</DialogTitle>
              <DialogDescription>
                {name} will no longer open this project. Their comments and decisions stay, with
                their name.
              </DialogDescription>
              <ErrorLine error={remove.error} />
              <DialogFooter>
                <Button
                  variant="destructive"
                  disabled={remove.isPending}
                  onClick={() => {
                    remove.mutate();
                  }}
                >
                  Remove from the project
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>
      ) : (
        <span className="text-sm text-muted-foreground">{label("role", member.role)}</span>
      )}
      <ErrorLine error={setRole.error} />
    </li>
  );
}

function AddMemberDialog({ project, onAdded }: { project: string; onAdded: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const ids = { email: useId(), role: useId(), hint: useId() };
  const query = email.trim();
  const users = useQuery({
    queryKey: ["users", query],
    enabled: open && query.length >= 3,
    queryFn: async () =>
      unwrap(await api.GET("/api/users", { params: { query: { email: query, limit: 10 } } })),
  });
  const add = useMutation({
    mutationFn: async (userId: string) =>
      unwrap(
        await api.PUT("/api/projects/{slug}/members/{user_id}", {
          params: { path: { slug: project, user_id: userId } },
          body: { role },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setEmail("");
      await onAdded();
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) add.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button>Add a member</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Add a member</DialogTitle>
        <DialogDescription>
          People appear here after they sign in once. Find them by email.
        </DialogDescription>
        <div className="flex flex-col gap-1.5">
          <label htmlFor={ids.email} className="text-sm font-medium">
            Email
          </label>
          <Input
            id={ids.email}
            type="search"
            value={email}
            aria-describedby={ids.hint}
            onChange={(event) => {
              setEmail(event.target.value);
            }}
          />
          <p id={ids.hint} className="text-xs text-muted-foreground">
            Type at least three characters.
          </p>
        </div>
        <div className="flex flex-col gap-1.5">
          <label htmlFor={ids.role} className="text-sm font-medium">
            Role
          </label>
          <NativeSelect
            id={ids.role}
            value={role}
            onChange={(event) => {
              setRole(event.target.value as Role);
            }}
          >
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {label("role", r)}
              </option>
            ))}
          </NativeSelect>
        </div>
        {query.length >= 3 ? (
          <QueryView query={users}>
            {(page) =>
              page.items.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  Nobody with that email has signed in.
                </p>
              ) : (
                <ul className="flex flex-col divide-y">
                  {page.items.map((u) => (
                    <li key={u.id} className="flex items-center justify-between gap-2 py-2">
                      <span className="text-sm">
                        {u.display_name ?? u.email}
                        {u.display_name && u.email ? (
                          <span className="text-muted-foreground"> · {u.email}</span>
                        ) : null}
                      </span>
                      <Button
                        size="sm"
                        disabled={add.isPending}
                        onClick={() => {
                          add.mutate(u.id);
                        }}
                      >
                        Add <span className="sr-only">{u.email ?? u.display_name}</span>
                      </Button>
                    </li>
                  ))}
                </ul>
              )
            }
          </QueryView>
        ) : null}
        <ErrorLine error={add.error} />
      </DialogContent>
    </Dialog>
  );
}

function ServiceAccountsSection({ project, canWrite }: { project: string; canWrite: boolean }) {
  const accounts = useServiceAccounts(project);
  const queryClient = useQueryClient();
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: [...projectKey(project), "service-accounts"] });
  };
  return (
    <Section
      title="Service accounts"
      description="Agents, experimenters, verifiers and deciders that work in this project with their own tokens."
      actions={
        canWrite ? <NewServiceAccountDialog project={project} onCreated={refresh} /> : undefined
      }
    >
      <QueryView query={accounts}>
        {(page) =>
          page.items.length === 0 ? (
            <EmptyState>No service account yet.</EmptyState>
          ) : (
            <ul className="flex flex-col divide-y">
              {page.items.map((account) => (
                <ServiceAccountRow
                  key={account.id}
                  project={project}
                  account={account}
                  canWrite={canWrite}
                  onChanged={refresh}
                />
              ))}
            </ul>
          )
        }
      </QueryView>
    </Section>
  );
}

function ServiceAccountRow({
  project,
  account,
  canWrite,
  onChanged,
}: {
  project: string;
  account: ServiceAccount;
  canWrite: boolean;
  onChanged: () => Promise<void>;
}) {
  const [showTokens, setShowTokens] = useState(false);
  const tokens = useServiceTokens(project, account.name, showTokens);
  const queryClient = useQueryClient();
  const refreshTokens = async () => {
    await queryClient.invalidateQueries({
      queryKey: [...projectKey(project), "service-accounts", account.name, "tokens"],
    });
  };
  const disabled = account.disabled_at !== null;
  return (
    <li className="flex flex-col gap-3 py-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-col">
          <span className="font-medium">{account.name}</span>
          <span className="text-xs text-muted-foreground">
            {label("serviceKind", account.kind)}
            {account.description ? ` · ${account.description}` : ""} · created{" "}
            {formatDate(account.created_at)}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <StatusChip domain="account" value={disabled ? "disabled" : "active"} />
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={showTokens}
            onClick={() => {
              setShowTokens(!showTokens);
            }}
          >
            {showTokens ? "Hide tokens" : "Tokens"}
          </Button>
          {canWrite && !disabled ? (
            <DisableServiceAccountDialog project={project} account={account} onDone={onChanged} />
          ) : null}
        </div>
      </div>
      {showTokens ? (
        <div className="flex flex-col gap-2 rounded-md border p-3">
          {canWrite && !disabled ? (
            <div>
              <NewTokenDialog
                trigger="New token"
                title={`New token for ${account.name}`}
                description="The service uses this token to call Cannery Row as itself."
                create={async (body) =>
                  unwrap(
                    await api.POST("/api/projects/{slug}/service-accounts/{name}/tokens", {
                      params: { path: { slug: project, name: account.name } },
                      body,
                    }),
                  )
                }
                onCreated={refreshTokens}
              />
            </div>
          ) : null}
          <QueryView query={tokens}>
            {(page) =>
              page.items.length === 0 ? (
                <p className="text-sm text-muted-foreground">No token.</p>
              ) : (
                <TokenRows tokens={page.items} canRevoke={canWrite} onRevoked={refreshTokens} />
              )
            }
          </QueryView>
        </div>
      ) : null}
    </li>
  );
}

function NewServiceAccountDialog({
  project,
  onCreated,
}: {
  project: string;
  onCreated: () => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState<"agent" | "experimenter" | "verifier" | "decider">("agent");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const ids = { kind: useId(), name: useId(), description: useId(), hint: useId() };
  const create = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/service-accounts", {
          params: { path: { slug: project } },
          body: { kind, name, description: description.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      setName("");
      setDescription("");
      await onCreated();
    },
  });
  const valid = SLUG.test(name);
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) create.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button>New service account</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>New service account</DialogTitle>
        <DialogDescription>
          An agent plans and tries units; an experimenter runs a workflow; a verifier re-runs
          results and applies the project's policy; a decider runs the decider step that decides
          written-up units when the science revision registers one. None of them can record a human
          decision.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (valid) create.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.kind} className="text-sm font-medium">
              Kind
            </label>
            <NativeSelect
              id={ids.kind}
              value={kind}
              onChange={(event) => {
                setKind(event.target.value as "agent" | "experimenter" | "verifier" | "decider");
              }}
            >
              {(["agent", "experimenter", "verifier", "decider"] as const).map((k) => (
                <option key={k} value={k}>
                  {label("serviceKind", k)}
                </option>
              ))}
            </NativeSelect>
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.name} className="text-sm font-medium">
              Name
            </label>
            <p id={ids.hint} className="text-xs text-muted-foreground">
              Lowercase letters, digits and dashes, for example codex.
            </p>
            <Input
              id={ids.name}
              value={name}
              aria-describedby={ids.hint}
              aria-invalid={name !== "" && !valid}
              onChange={(event) => {
                setName(event.target.value);
              }}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <label htmlFor={ids.description} className="text-sm font-medium">
              Description
            </label>
            <Input
              id={ids.description}
              value={description}
              onChange={(event) => {
                setDescription(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={create.error} />
          <DialogFooter>
            <Button type="submit" disabled={!valid || create.isPending}>
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DisableServiceAccountDialog({
  project,
  account,
  onDone,
}: {
  project: string;
  account: ServiceAccount;
  onDone: () => Promise<void>;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const disable = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/projects/{slug}/service-accounts/{name}/disable", {
          params: { path: { slug: project, name: account.name } },
          body: { reason: reason.trim() },
        }),
      ),
    onSuccess: async () => {
      setOpen(false);
      await onDone();
    },
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) disable.reset();
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline" size="sm">
          Disable <span className="sr-only">{account.name}</span>
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>Disable {account.name}?</DialogTitle>
        <DialogDescription>
          Its tokens stop working and it can no longer act in this project. Its past work stays.
        </DialogDescription>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (reason.trim()) disable.mutate();
          }}
        >
          <div className="flex flex-col gap-1.5">
            <label htmlFor={id} className="text-sm font-medium">
              Reason <span className="font-normal text-muted-foreground">(required)</span>
            </label>
            <Textarea
              id={id}
              value={reason}
              onChange={(event) => {
                setReason(event.target.value);
              }}
            />
          </div>
          <ErrorLine error={disable.error} />
          <DialogFooter>
            <Button
              type="submit"
              variant="destructive"
              disabled={!reason.trim() || disable.isPending}
            >
              Disable
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
