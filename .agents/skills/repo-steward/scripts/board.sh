#!/usr/bin/env bash
# The steward's Kanban board: a GitHub project found by its title (profile board.title).
#
#   board.sh ensure [--dry-run]  find or create the project; link it to the repository; set its
#                                visibility, description and policies README; create missing
#                                fields, Status options and views. Every step is skipped when
#                                already in place; nothing is ever deleted.
#   board.sh ids                 print the ids a run needs as key=value lines (reads only)
#   board.sh item <url> [Field=Value ...] [--archive] [--dry-run]
#                                add an issue or pull request to the board (repeating is safe)
#                                and set fields by name: a single-select option by its name, a
#                                date as YYYY-MM-DD, a text; an empty value clears the field
#   board.sh status-update <text> [--dry-run]
#                                post a project status update
#   board.sh apply <queue file>  carry out the changes a queue file holds, checking each again
#
# --dry-run prints every call that would write instead of making it.
#
# In the unattended workflow the agent holds no token that reaches Projects. There
# STEWARD_BOARD_QUEUE names a file: `item` and `status-update` check their arguments, append them
# to it and touch nothing else; a later step that holds STEWARD_PROJECT_TOKEN runs `apply`.
set -euo pipefail
# Git for Windows' bash rewrites arguments that look like paths ("ref:dir/file") unless told not to.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
[ -z "${STEWARD_PROJECT_TOKEN:-}" ] || export GH_TOKEN="$STEWARD_PROJECT_TOKEN"
queue=${STEWARD_BOARD_QUEUE:-}

here=$(cd "$(dirname "$0")" && pwd)
command=${1:-}
[ $# -eq 0 ] || shift
dry_run=0
args=()
for arg in "$@"; do
	if [ "$arg" = --dry-run ]; then dry_run=1; else args+=("$arg"); fi
done

p() { bash "$here/profile.sh" get "$1"; }
strip() { tr -d '\r'; }

# A JSON string literal for $1.
json() {
	printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g' |
		awk '{ printf "%s%s", (NR > 1 ? "\\n" : ""), $0 }')"
}

run() {
	if [ "$dry_run" = 1 ]; then
		printf 'would run:'
		printf ' %q' "$@"
		printf '\n'
	else
		"$@" >/dev/null
	fi
}

repo=$(bash "$here/context.sh" get repo)
owner=${repo%%/*}
name=${repo#*/}
title=$(p board.title | sed "s#{repo}#$name#g")

# REST and GraphQL paths differ for organization and personal projects. Only commands that
# reach GitHub call this; queueing works offline.
remote() {
	local owner_type
	owner_type=$(gh api "users/$owner" --jq .type | strip)
	if [ "$owner_type" = Organization ]; then
		rest_base="orgs/$owner/projectsV2"
		graphql_owner=organization
	else
		rest_base="users/$(gh api "users/$owner" --jq .id | strip)/projectsV2"
		graphql_owner=user
	fi
}

# Checks one Field=Value pair against the board's known fields, without contacting GitHub.
valid_pair() {
	local field=${1%%=*} value=${1#*=} extra
	case "$1" in *=*) ;; *) echo "board.sh: '$1' is not Field=Value" >&2; return 1 ;; esac
	case "$value" in *$'\t'* | *$'\n'* | *$'\r'*) echo "board.sh: the value of $field must be one line" >&2; return 1 ;; esac
	[ "${#value}" -le 200 ] || { echo "board.sh: the value of $field is longer than 200 characters" >&2; return 1; }
	case "$field" in
	Status)
		case "$value" in Triage | Backlog | Ready | "In Progress" | "In Review" | Done) return 0 ;; esac
		echo "board.sh: Status has no option '$value'" >&2
		return 1
		;;
	Priority) case "$value" in P0 | P1 | P2 | P3) return 0 ;; esac ;;
	Kind) case "$value" in Bug | Feature | Docs | Question | Chore) return 0 ;; esac ;;
	Started | Finished)
		case "$value" in "" | [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) return 0 ;; esac
		echo "board.sh: $field must be a date as YYYY-MM-DD" >&2
		return 1
		;;
	Blocked) return 0 ;;
	*)
		for extra in $(p board.text_fields); do
			[ "$field" = "$extra" ] && return 0
		done
		echo "board.sh: the board has no field '$field'" >&2
		return 1
		;;
	esac
	echo "board.sh: $field has no option '$value'" >&2
	return 1
}

valid_url() {
	case "$1" in
	"https://github.com/$repo/issues/"[0-9]* | "https://github.com/$repo/pull/"[0-9]*)
		case "${1##*/}" in *[!0-9]*) ;; *) return 0 ;; esac
		;;
	esac
	echo "board.sh: '$1' is not an issue or pull request of $repo" >&2
	return 1
}

find_project() {
	gh project list --owner "$owner" --format json --limit 100 \
		--jq ".projects[] | select(.title == $(json "$title")) | .number" | strip | head -n 1
}

readme() {
	sed -e '/markdownlint-disable/d' \
		-e "s#{repo}#$repo#g" \
		-e "s#{wip_in_progress}#$(p board.wip_in_progress)#g" \
		-e "s#{wip_in_review}#$(p board.wip_in_review)#g" \
		-e "s#{done_retention_days}#$(p board.done_retention_days)#g" \
		"$here/../templates/board-readme.md"
}

visibility() {
	local wanted
	wanted=$(p board.public)
	case "$wanted" in
	true) echo PUBLIC ;;
	false) echo PRIVATE ;;
	*)
		if [ "$(gh repo view "$repo" --json visibility --jq .visibility | strip)" = PUBLIC ]; then
			echo PUBLIC
		else
			echo PRIVATE
		fi
		;;
	esac
}

# "name<TAB>id<TAB>type" per field of the project.
fields() {
	gh project field-list "$1" --owner "$owner" --format json --limit 100 \
		--jq '.fields[] | "\(.name)\t\(.id)\t\(.type)"' | strip
}

# Status options in board order: name|colour|description.
status_options() {
	cat <<'EOF'
Triage|PURPLE|Opened or reopened; waiting for type and priority
Backlog|GRAY|Accepted and prioritized, or waiting for information
Ready|BLUE|Reproduced or specified and unblocked; can start now
In Progress|YELLOW|Work started; counts against the WIP limit
In Review|ORANGE|A pull request is open; counts against the WIP limit
Done|GREEN|Merged or resolved
EOF
}

ensure_status_options() {
	local number=$1 field_id existing body names wanted kept="" first=1 option_name colour description id
	field_id=$(gh project field-list "$number" --owner "$owner" --format json --limit 100 \
		--jq '.fields[] | select(.name == "Status") | .id' | strip)
	existing=$(gh project field-list "$number" --owner "$owner" --format json --limit 100 \
		--jq '.fields[] | select(.name == "Status") | .options[] | "\(.name)\t\(.id)"' | strip)
	names=$(printf '%s\n' "$existing" | cut -f 1)
	wanted=$(status_options | cut -d '|' -f 1)
	local absent=""
	while IFS= read -r option_name; do
		printf '%s\n' "$names" | grep -Fqx -- "$option_name" || absent="$absent${absent:+, }$option_name"
	done <<EOF
$wanted
EOF
	if [ -z "$absent" ]; then
		echo "Status options already in place"
		return
	fi
	body='{"query":"mutation($field: ID!, $options: [ProjectV2SingleSelectFieldOptionInput!]) { updateProjectV2Field(input: {fieldId: $field, singleSelectOptions: $options}) { projectV2Field { ... on ProjectV2SingleSelectField { id } } } }","variables":{"field":'"$(json "$field_id")"',"options":['
	while IFS='|' read -r option_name colour description; do
		id=$(printf '%s\n' "$existing" | awk -F '\t' -v want="$option_name" '$1 == want { print $2 }')
		# A new project's "Todo" becomes "Backlog", so items keep their place.
		if [ -z "$id" ] && [ "$option_name" = Backlog ]; then
			id=$(printf '%s\n' "$existing" | awk -F '\t' '$1 == "Todo" { print $2 }')
			[ -z "$id" ] || kept="$kept Todo"
		fi
		[ "$first" = 1 ] || body="$body,"
		first=0
		body="$body{\"name\":$(json "$option_name"),\"color\":\"$colour\",\"description\":$(json "$description")"
		[ -z "$id" ] || body="$body,\"id\":$(json "$id")"
		body="$body}"
		kept="$kept $option_name"
	done <<EOF
$(status_options)
EOF
	# Options a maintainer added are kept: dropping one would clear it from every item.
	while IFS="$(printf '\t')" read -r option_name id; do
		[ -n "$option_name" ] || continue
		case " $kept " in *" $option_name "*) continue ;; esac
		body="$body,{\"name\":$(json "$option_name"),\"color\":\"GRAY\",\"description\":\"\",\"id\":$(json "$id")}"
	done <<EOF
$existing
EOF
	body="$body]}}"
	if [ "$dry_run" = 1 ]; then
		printf 'would run: gh api graphql --input - <<< %s\n' "$body"
	else
		printf '%s' "$body" | gh api graphql --input - >/dev/null
		echo "Status options set"
	fi
}

ensure_views() {
	local number=$1 present ids status priority finished
	present=$(gh api graphql -f query="query(\$login: String!, \$number: Int!) { $graphql_owner(login: \$login) { projectV2(number: \$number) { views(first: 50) { nodes { name } } } } }" \
		-f login="$owner" -F number="$number" --jq ".data.$graphql_owner.projectV2.views.nodes[].name" | strip)
	ids=$(gh api "$rest_base/$number/fields" --jq '.[] | "\(.name)\t\(.id)"' | strip)
	status=$(printf '%s\n' "$ids" | awk -F '\t' '$1 == "Status" { print $2 }')
	priority=$(printf '%s\n' "$ids" | awk -F '\t' '$1 == "Priority" { print $2 }')
	finished=$(printf '%s\n' "$ids" | awk -F '\t' '$1 == "Finished" { print $2 }')
	view() {
		local view_name=$1 json_body=$2
		if printf '%s\n' "$present" | grep -Fqx -- "$view_name"; then
			echo "view present: $view_name"
		elif [ "$dry_run" = 1 ]; then
			printf 'would run: gh api -X POST %s --input - <<< %s\n' "$rest_base/$number/views" "$json_body"
		else
			printf '%s' "$json_body" | gh api -X POST "$rest_base/$number/views" --input - >/dev/null
			echo "view created: $view_name"
		fi
	}
	view Flow "{\"name\":\"Flow\",\"layout\":\"board\",\"vertical_group_by\":[${status:-0}],\"sort_by\":[[${priority:-0},\"asc\"]]}"
	view "Triage queue" '{"name":"Triage queue","layout":"table","filter":"status:Triage"}'
	view "Awaiting maintainer" '{"name":"Awaiting maintainer","layout":"table","filter":"label:\"status: needs-maintainer\""}'
	view Finished "{\"name\":\"Finished\",\"layout\":\"table\",\"filter\":\"status:Done\",\"sort_by\":[[${finished:-0},\"desc\"]]}"
}

ensure() {
	local number present field type options extra
	number=$(find_project)
	if [ -z "$number" ]; then
		if [ "$dry_run" = 1 ]; then
			echo "no project titled \"$title\" for $owner"
			run gh project create --owner "$owner" --title "$title" --format json
			run gh project link "<number>" --owner "$owner" --repo "$repo"
			run gh project edit "<number>" --owner "$owner" --visibility "$(visibility)" \
				--description "Issues and pull requests of $repo, kept by the repo steward" --readme "$(readme)"
			for field in "Priority SINGLE_SELECT P0,P1,P2,P3" "Kind SINGLE_SELECT Bug,Feature,Docs,Question,Chore" \
				"Started DATE" "Finished DATE" "Blocked TEXT"; do
				set -- $field
				if [ -n "${3:-}" ]; then
					run gh project field-create "<number>" --owner "$owner" --name "$1" --data-type "$2" --single-select-options "$3"
				else
					run gh project field-create "<number>" --owner "$owner" --name "$1" --data-type "$2"
				fi
			done
			for extra in $(p board.text_fields); do
				run gh project field-create "<number>" --owner "$owner" --name "$extra" --data-type TEXT
			done
			echo "would set Status options: $(status_options | cut -d '|' -f 1 | paste -sd ',' -)"
			echo "would create views: Flow (board by Status), Triage queue, Awaiting maintainer, Finished"
			return
		fi
		number=$(gh project create --owner "$owner" --title "$title" --format json --jq .number | strip)
		echo "created project #$number \"$title\""
	else
		echo "found project #$number \"$title\""
	fi
	# Linking an already linked repository fails; that failure means the step is done.
	if [ "$dry_run" = 1 ]; then
		run gh project link "$number" --owner "$owner" --repo "$repo"
	else
		gh project link "$number" --owner "$owner" --repo "$repo" >/dev/null 2>&1 || true
	fi
	run gh project edit "$number" --owner "$owner" --visibility "$(visibility)" \
		--description "Issues and pull requests of $repo, kept by the repo steward" --readme "$(readme)"
	present=$(fields "$number" | cut -f 1)
	while read -r field type options; do
		[ -n "$field" ] || continue
		printf '%s\n' "$present" | grep -Fqx -- "$field" && continue
		if [ -n "$options" ]; then
			run gh project field-create "$number" --owner "$owner" --name "$field" --data-type "$type" --single-select-options "$options"
		else
			run gh project field-create "$number" --owner "$owner" --name "$field" --data-type "$type"
		fi
		[ "$dry_run" = 1 ] || echo "field created: $field"
	done <<EOF
Priority SINGLE_SELECT P0,P1,P2,P3
Kind SINGLE_SELECT Bug,Feature,Docs,Question,Chore
Started DATE
Finished DATE
Blocked TEXT
$(for extra in $(p board.text_fields); do printf '%s TEXT\n' "$extra"; done)
EOF
	ensure_status_options "$number"
	ensure_views "$number"
}

ids() {
	local number
	number=$(find_project)
	if [ -z "$number" ]; then
		echo "board.sh: no project titled \"$title\" for $owner; run board.sh ensure" >&2
		exit 1
	fi
	printf 'project_number=%s\nproject_owner=%s\n' "$number" "$owner"
	printf 'project_id=%s\n' "$(gh project view "$number" --owner "$owner" --format json --jq .id | strip)"
	gh project field-list "$number" --owner "$owner" --format json --limit 100 \
		--jq '.fields[] | "field_\(.name | gsub("[^A-Za-z0-9]"; "_"))=\(.id)", (.options[]? as $o | "option_\(.name | gsub("[^A-Za-z0-9]"; "_"))_\($o.name | gsub("[^A-Za-z0-9]"; "_"))=\($o.id)")' | strip
}

require_project() {
	local number
	number=$(find_project)
	if [ -z "$number" ]; then
		echo "board.sh: no project titled \"$title\" for $owner; run board.sh ensure" >&2
		exit 1
	fi
	printf '%s\n' "$number"
}

# board.sh item <url> [Field=Value ...] [--archive]
item() {
	local url=${1:?board.sh item needs an issue or pull request URL} number project_id item_id
	local list pair field value line field_id type option_id archive=0
	shift
	valid_url "$url" || exit 2
	for pair in "$@"; do
		[ "$pair" = --archive ] || valid_pair "$pair" || exit 2
	done
	if [ -n "$queue" ]; then
		{
			printf 'item\t%s' "$url"
			for pair in "$@"; do printf '\t%s' "$pair"; done
			printf '\n'
		} >>"$queue"
		echo "queued board change for $url"
		return
	fi
	number=$(require_project)
	project_id=$(gh project view "$number" --owner "$owner" --format json --jq .id | strip)
	if [ "$dry_run" = 1 ]; then
		item_id="<item id>"
		run gh project item-add "$number" --owner "$owner" --url "$url"
	else
		item_id=$(gh project item-add "$number" --owner "$owner" --url "$url" --format json --jq .id | strip)
	fi
	list=$(gh project field-list "$number" --owner "$owner" --format json --limit 100 \
		--jq '.fields[] | "\(.name)\t\(.id)\t\(.type)\t\([.options[]? | "\(.name)=\(.id)"] | join(";"))"' | strip)
	for pair in "$@"; do
		if [ "$pair" = --archive ]; then
			archive=1
			continue
		fi
		field=${pair%%=*}
		value=${pair#*=}
		line=$(printf '%s\n' "$list" | awk -F '\t' -v want="$field" '$1 == want { print; exit }')
		if [ -z "$line" ]; then
			echo "board.sh: the board has no field \"$field\"; run board.sh ensure" >&2
			exit 1
		fi
		field_id=$(printf '%s\n' "$line" | cut -f 2)
		type=$(printf '%s\n' "$line" | cut -f 3)
		if [ -z "$value" ]; then
			run gh project item-edit --id "$item_id" --project-id "$project_id" --field-id "$field_id" --clear
			continue
		fi
		case "$type" in
		ProjectV2SingleSelectField)
			option_id=$(printf '%s\n' "$line" | cut -f 4 | tr ';' '\n' | awk -F '=' -v want="$value" '$1 == want { print $2; exit }')
			if [ -z "$option_id" ]; then
				echo "board.sh: field \"$field\" has no option \"$value\"" >&2
				exit 1
			fi
			run gh project item-edit --id "$item_id" --project-id "$project_id" --field-id "$field_id" --single-select-option-id "$option_id"
			;;
		*)
			case "$value" in
			[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9])
				run gh project item-edit --id "$item_id" --project-id "$project_id" --field-id "$field_id" --date "$value"
				;;
			*) run gh project item-edit --id "$item_id" --project-id "$project_id" --field-id "$field_id" --text "$value" ;;
			esac
			;;
		esac
	done
	[ "$archive" = 0 ] || run gh project item-archive "$number" --owner "$owner" --id "$item_id"
	[ "$dry_run" = 1 ] || echo "board item $item_id updated"
}

status_update() {
	local text=${1:?board.sh status-update needs the text} number project_id
	[ "${#text}" -le 2000 ] || { echo "board.sh: a status update is at most 2000 characters" >&2; exit 2; }
	case "$text" in *$'\t'* | *$'\r'*) echo "board.sh: a status update has no tabs" >&2; exit 2 ;; esac
	if [ -n "$queue" ]; then
		# One queue line per update: newlines are stored as the two characters \n.
		printf 'status\t%s\n' "$(printf '%s' "$text" | awk '{ printf "%s%s", (NR > 1 ? "\\n" : ""), $0 }')" >>"$queue"
		echo "queued status update"
		return
	fi
	number=$(require_project)
	project_id=$(gh project view "$number" --owner "$owner" --format json --jq .id | strip)
	run gh api graphql \
		-f query='mutation($project: ID!, $body: String!) { createProjectV2StatusUpdate(input: {projectId: $project, body: $body}) { statusUpdate { id } } }' \
		-f project="$project_id" -f body="$text"
	[ "$dry_run" = 1 ] || echo "status update posted"
}

# Carries out a queue file written by an agent run. Every line is checked again here, so a line
# the agent could not have written through `item` or `status-update` is skipped, not run.
apply() {
	local file=${1:?board.sh apply needs the queue file} line kind rest text line_number=0
	[ -f "$file" ] || { echo "nothing queued"; return; }
	queue=""
	while IFS= read -r line || [ -n "$line" ]; do
		line_number=$((line_number + 1))
		kind=${line%%$'\t'*}
		rest=${line#*$'\t'}
		case "$kind" in
		item)
			local -a fields=()
			IFS=$'\t' read -r -a fields <<<"$rest"
			(item "${fields[@]}") || echo "board.sh: queue line $line_number skipped" >&2
			;;
		status)
			text=$(printf '%s\n' "$rest" | awk '{ gsub(/\\n/, "\n"); print }')
			(status_update "$text") || echo "board.sh: queue line $line_number skipped" >&2
			;;
		*) echo "board.sh: queue line $line_number is neither item nor status; skipped" >&2 ;;
		esac
	done <"$file"
}

case "$command" in
ensure | ids)
	if [ -n "$queue" ]; then
		echo "board.sh: $command needs Projects access; it runs in an attended run (changes are queued here)"
		exit 0
	fi
	remote
	"$command"
	;;
item)
	[ -n "$queue" ] || remote
	item ${args[@]+"${args[@]}"}
	;;
status-update)
	[ -n "$queue" ] || remote
	status_update ${args[@]+"${args[@]}"}
	;;
apply)
	# The agent writes the queue; only the step that holds the Projects token may apply it.
	if [ -n "$queue" ]; then
		echo "board.sh: apply runs only where STEWARD_BOARD_QUEUE is not set" >&2
		exit 2
	fi
	remote
	apply ${args[@]+"${args[@]}"}
	;;
*)
	echo "usage: board.sh ensure [--dry-run] | ids | item <url> [Field=Value ...] [--archive] [--dry-run] | status-update <text> [--dry-run] | apply <queue file>" >&2
	exit 2
	;;
esac
