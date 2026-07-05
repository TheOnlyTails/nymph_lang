import * as option from '../option.js';
import { Option } from '../option.js';
class Node {
	constructor(prev, next, value) {
		this.prev = prev;
		this.next = next;
		this.value = value;
	}
}
export class LinkedList {
	constructor(head, tail, length) {
		this.head = head;
		this.tail = tail;
		this.length = length;
	}
}
