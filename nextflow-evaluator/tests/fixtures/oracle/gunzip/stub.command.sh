#!/bin/bash -ue
touch sample.fastq

# capture process environment
set +u
set +e
cd "$NXF_TASK_WORKDIR"

nxf_eval_cmd() {
    {
        IFS=$'\n' read -r -d '' "${1}";
        IFS=$'\n' read -r -d '' "${2}";
        (IFS=$'\n' read -r -d '' _ERRNO_; return ${_ERRNO_});
    } < <((printf '\0%s\0%d\0' "$(((({ shift 2; "${@}"; echo "${?}" 1>&3-; } | tr -d '\0' 1>&4-) 4>&2- 2>&1- | tr -d '\0' 1>&4-) 3>&1- | exit "$(cat)") 4>&1-)" "${?}" 1>&2) 2>&1)
}

echo '' > .command.env
#
nxf_eval_cmd STDOUT STDERR bash -c "gunzip --version 2>&1 | head -1 | sed \"s/^.*(gzip) //; s/ Copyright.*//\""
status=$?
if [ $status -eq 0 ]; then
  echo nxf_out_eval_1="$STDOUT" >> .command.env
  echo /nxf_out_eval_1/=exit:0 >> .command.env
else
  echo nxf_out_eval_1="$STDERR" >> .command.env
  echo /nxf_out_eval_1/=exit:$status >> .command.env
fi
