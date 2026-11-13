# uwe's launcher of nf-r-ipc calls: reads the request table, calls the R function, writes the answer.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

nfr_control <- "__nfr_control__"
nfr_data <- "__nfr_data__"

# nanoarrow from the image, else installed once into the library folder of the work directory.
nfr_nanoarrow <- function() {
  if (requireNamespace("nanoarrow", quietly = TRUE)) return(invisible(TRUE))
  library_path <- Sys.getenv("NFR_LIBRARY")
  if (nzchar(library_path)) {
    dir.create(library_path, recursive = TRUE, showWarnings = FALSE)
    .libPaths(c(library_path, .libPaths()))
    if (!requireNamespace("nanoarrow", quietly = TRUE)) {
      repos <- getOption("repos")
      if (is.null(repos) || identical(unname(repos["CRAN"]), "@CRAN@")) {
        repos <- c(CRAN = "https://cloud.r-project.org")
      }
      utils::install.packages("nanoarrow", lib = library_path, repos = repos, quiet = TRUE)
    }
  }
  if (!requireNamespace("nanoarrow", quietly = TRUE)) stop("the R package nanoarrow is not installed")
  invisible(TRUE)
}

nfr_text <- function(value) {
  if (is.null(value) || length(value) == 0 || is.na(value)) NA_character_ else as.character(value)
}

nfr_key <- function(id) format(id, scientific = FALSE, trim = TRUE)

# The R value of the value rows: NULL, NA of a type, scalars, lists, named lists, data frames.
nfr_decode <- function(rows) {
  data <- rows[rows$section %in% nfr_data, , drop = FALSE]
  ids <- as.numeric(data$value_id)
  parents <- as.numeric(data$parent_id)
  roots <- which(is.na(parents))
  if (length(roots) != 1) stop("Expected exactly one root value node")
  children <- split(seq_len(nrow(data)), nfr_key(ifelse(is.na(parents), -1, parents)))
  build <- function(place) {
    kids <- children[[nfr_key(ids[place])]]
    if (is.null(kids)) kids <- integer(0)
    tag <- data$tag[place]
    if (tag == "null") return(NULL)
    if (tag == "na_logical") return(NA)
    if (tag == "na_integer") return(NA_integer_)
    if (tag == "na_double") return(NA_real_)
    if (tag == "na_character") return(NA_character_)
    if (tag == "string") return(as.character(data$v_string[place]))
    if (tag == "int64") return(as.numeric(data$v_int64[place]))
    if (tag == "float64") return(as.numeric(data$v_float64[place]))
    if (tag == "bool") return(as.logical(data$v_bool[place]))
    if (tag == "list") {
      kids <- kids[order(as.numeric(data$index[kids]))]
      return(lapply(kids, build))
    }
    values <- lapply(kids, build)
    names(values) <- data$key[kids]
    if (tag == "map") return(if (length(kids) == 0) list() else values)
    if (tag == "data_frame") {
      if (length(values) == 0) return(data.frame())
      columns <- lapply(values, function(column) {
        unlist(lapply(column, function(cell) if (is.null(cell)) NA else cell))
      })
      return(as.data.frame(columns, stringsAsFactors = FALSE, check.names = FALSE))
    }
    stop(sprintf("Unknown value graph tag: %s", tag))
  }
  build(roots)
}

# A list of named lists as a data frame, columns in first-seen order; other values as they are.
nfr_records <- function(value) {
  if (!is.list(value) || is.data.frame(value) || !is.null(names(value)) || length(value) == 0) {
    return(value)
  }
  named <- vapply(value, function(item) is.list(item) && !is.data.frame(item) && !is.null(names(item)), logical(1))
  if (!all(named)) return(value)
  columns <- unique(unlist(lapply(value, names)))
  frame <- lapply(columns, function(column) {
    unlist(lapply(value, function(item) {
      cell <- item[[column]]
      if (is.null(cell) || length(cell) != 1) NA else cell
    }))
  })
  names(frame) <- columns
  as.data.frame(frame, stringsAsFactors = FALSE, check.names = FALSE)
}

# Times, factors and data frames as plain values, at every depth.
nfr_normal <- function(value) {
  if (is.null(value)) return(NULL)
  if (inherits(value, "difftime")) return(as.numeric(value, units = "secs"))
  if (is.factor(value)) return(as.character(value))
  if (inherits(value, "Date")) return(format(value, "%Y-%m-%d"))
  if (inherits(value, "POSIXt")) {
    return(format(as.POSIXct(value), "%Y-%m-%d %H:%M:%S UTC", tz = "UTC"))
  }
  if (is.data.frame(value)) {
    return(lapply(seq_len(nrow(value)), function(place) {
      row <- lapply(names(value), function(column) nfr_normal(value[[column]][[place]]))
      names(row) <- names(value)
      row
    }))
  }
  if (is.list(value)) {
    normal <- lapply(value, nfr_normal)
    names(normal) <- names(value)
    return(normal)
  }
  value
}

nfr_scalar <- function(value) is.null(value) || (is.atomic(value) && length(value) == 1)

nfr_columns <- function(value) {
  if (length(value) == 0) return(FALSE)
  shaped <- vapply(value, function(column) {
    is.list(column) && is.null(names(column)) && all(vapply(column, nfr_scalar, logical(1)))
  }, logical(1))
  all(shaped) && length(unique(lengths(value))) == 1
}

# The value rows of a result, from id 1 in depth-first order.
nfr_encode <- function(result) {
  nodes <- new.env()
  nodes$rows <- list()
  nodes$last <- 0
  add <- function(value, parent, key, index) {
    nodes$last <- nodes$last + 1
    id <- nodes$last
    row <- list(value_id = id, parent_id = parent, key = key, index = index, tag = NA_character_,
                v_string = NA_character_, v_int64 = NA_real_, v_float64 = NA_real_, v_bool = NA)
    place <- length(nodes$rows) + 1
    nodes$rows[[place]] <- row
    set <- function(name, item) nodes$rows[[place]][[name]] <- item
    if (is.null(value)) return(set("tag", "null"))
    if (is.list(value)) {
      names_of <- names(value)
      if (is.null(names_of)) {
        set("tag", "list")
        for (at in seq_along(value)) add(value[[at]], id, NA_character_, at - 1L)
      } else if (nfr_columns(value)) {
        set("tag", "data_frame")
        for (name in names_of) add(value[[name]], id, name, NA_integer_)
      } else {
        set("tag", "map")
        for (at in seq_along(value)) add(value[[at]], id, names_of[[at]], NA_integer_)
      }
      return(invisible())
    }
    if (is.atomic(value) && length(value) != 1) {
      set("tag", "list")
      for (at in seq_along(value)) add(value[[at]], id, NA_character_, at - 1L)
      return(invisible())
    }
    if (!is.atomic(value)) {
      stop(sprintf("Unsupported output value type: %s", paste(class(value), collapse = "/")))
    }
    kind <- typeof(value)
    if (is.na(value) && !is.nan(value)) {
      tags <- c(logical = "na_logical", integer = "na_integer", double = "na_double", character = "na_character")
      if (is.na(tags[kind])) stop(sprintf("Unsupported output value type: %s", paste(class(value), collapse = "/")))
      return(set("tag", unname(tags[kind])))
    }
    if (kind == "character") {
      set("tag", "string")
      return(set("v_string", value))
    }
    if (kind == "logical") {
      set("tag", "bool")
      return(set("v_bool", value))
    }
    if (kind == "integer") {
      set("tag", "int64")
      return(set("v_int64", as.numeric(value)))
    }
    if (kind == "double") {
      set("tag", "float64")
      return(set("v_float64", value))
    }
    stop(sprintf("Unsupported output value type: %s", paste(class(value), collapse = "/")))
  }
  add(result, NA_real_, NA_character_, NA_integer_)
  nodes$rows
}

# The answer table: the control row, then the value rows, with the request's control echoed.
nfr_table <- function(control, nodes, status, error_message) {
  count <- length(nodes) + 1
  column <- function(name, empty, first) {
    values <- rep(empty, count)
    values[1] <- first
    if (length(nodes) > 0) values[-1] <- vapply(nodes, function(row) row[[name]], empty)
    values
  }
  echo <- function(name) nfr_text(control[[name]][1])
  data.frame(
    section = c(nfr_control, rep(nfr_data, length(nodes))),
    protocol_version = c(1L, rep(NA_integer_, length(nodes))),
    call_id = c(echo("call_id"), rep(NA_character_, length(nodes))),
    script_mode = c(echo("script_mode"), rep(NA_character_, length(nodes))),
    script_ref = c(echo("script_ref"), rep(NA_character_, length(nodes))),
    payload_kind = c(echo("payload_kind"), rep(NA_character_, length(nodes))),
    status = c(status, rep(NA_character_, length(nodes))),
    result_kind = c(if (status == "ok") "value_graph" else NA_character_, rep(NA_character_, length(nodes))),
    error_class = c(if (status == "ok") NA_character_ else "RRuntimeError", rep(NA_character_, length(nodes))),
    error_message = c(nfr_text(error_message), rep(NA_character_, length(nodes))),
    value_id = column("value_id", NA_real_, NA_real_),
    parent_id = column("parent_id", NA_real_, NA_real_),
    key = column("key", NA_character_, NA_character_),
    index = column("index", NA_integer_, NA_integer_),
    tag = column("tag", NA_character_, NA_character_),
    v_string = column("v_string", NA_character_, NA_character_),
    v_int64 = column("v_int64", NA_real_, NA_real_),
    v_float64 = column("v_float64", NA_real_, NA_real_),
    v_bool = column("v_bool", NA, NA),
    `function` = c(echo("function"), rep(NA_character_, length(nodes))),
    stringsAsFactors = FALSE,
    check.names = FALSE
  )
}

# Sources the script, runs the inline code and calls the function with the arguments.
nfr_answer <- function(request, control) {
  arguments <- nfr_decode(request)
  if (identical(nfr_text(control$script_mode[1]), "path")) {
    path <- nfr_text(control$script_ref[1])
    if (is.na(path) || !file.exists(path)) stop(sprintf("Script path not found: %s", path))
    source(path, local = globalenv())
  }
  code_file <- Sys.getenv("NFR_INLINE_CODE_FILE")
  if (nzchar(code_file)) {
    if (!file.exists(code_file)) stop(sprintf("NFR_INLINE_CODE_FILE not found: %s", code_file))
    eval(parse(file = code_file), envir = globalenv())
  } else if (nzchar(Sys.getenv("NFR_INLINE_CODE"))) {
    eval(parse(text = Sys.getenv("NFR_INLINE_CODE")), envir = globalenv())
  }
  name <- nfr_text(control[["function"]][1])
  if (is.na(name) || !nzchar(name)) return(arguments)
  if (!exists(name, envir = globalenv(), mode = "function")) {
    stop(sprintf("Function not found in R session: %s", name))
  }
  call <- get(name, envir = globalenv(), mode = "function")
  arguments <- lapply(arguments, nfr_records)
  do.call(call, if (length(arguments) == 0) list() else arguments)
}

nfr_main <- function() {
  request_path <- Sys.getenv("NFR_REQUEST_IPC")
  response_path <- Sys.getenv("NFR_RESPONSE_IPC")
  if (!nzchar(request_path) || !nzchar(response_path)) {
    stop("NFR_REQUEST_IPC and NFR_RESPONSE_IPC must be set")
  }
  nfr_nanoarrow()
  request <- as.data.frame(nanoarrow::read_nanoarrow(request_path))
  control <- request[request$section %in% nfr_control, , drop = FALSE]
  if (nrow(control) < 1) stop("Request is missing __nfr_control__ row")
  answer <- tryCatch(
    list(status = "ok", nodes = nfr_encode(nfr_normal(nfr_answer(request, control)))),
    error = function(error) {
      list(status = "error", message = conditionMessage(error), nodes = nfr_encode(NULL))
    }
  )
  frame <- nfr_table(control, answer$nodes, answer$status, answer$message)
  nanoarrow::write_nanoarrow(frame, response_path)
}

nfr_main()
