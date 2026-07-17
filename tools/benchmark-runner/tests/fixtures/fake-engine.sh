case "$(cat "$1")" in
  *timeout*)
    printf 'timeout stdout'
    printf 'timeout stderr' >&2
    exec sleep 2
    ;;
  *failure*)
    printf 'fixture stdout that is deliberately longer than the configured cap'
    printf 'fixture stderr' >&2
    exit 7
    ;;
  *)
    printf 'fixture success'
    ;;
esac
